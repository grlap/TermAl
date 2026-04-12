/*
Backend regression tests
Coverage is organized around production seams rather than tiny private helpers:
  - HTTP router behavior
  - state mutation and persistence
  - runtime protocol normalization
  - remote/orchestrator integration helpers
The local fixtures below keep tests close to real wiring so include!-based
refactors still exercise the same cross-file behavior the app depends on.
*/

use super::*;
use axum::body::{Body, to_bytes};
use axum::http::Request;
use std::io::Read as _;
use tower::util::ServiceExt;

#[derive(Default)]
struct TestRecorder {
    approvals: Vec<(String, String, String)>,
    codex_approvals: Vec<(String, String, String, CodexPendingApproval)>,
    codex_user_input_requests: Vec<(
        String,
        String,
        Vec<UserInputQuestion>,
        CodexPendingUserInput,
    )>,
    codex_mcp_elicitation_requests: Vec<(
        String,
        String,
        McpElicitationRequestPayload,
        CodexPendingMcpElicitation,
    )>,
    codex_app_requests: Vec<(String, String, String, Value, CodexPendingAppRequest)>,
    commands: Vec<(String, String, CommandStatus)>,
    diffs: Vec<(String, String, String, ChangeType)>,
    parallel_agents: Vec<Vec<ParallelAgentProgress>>,
    subagent_results: Vec<(String, String)>,
    thinking: Vec<(String, Vec<String>)>,
    texts: Vec<String>,
    text_deltas: Vec<String>,
    streaming_text_delta_start: Option<usize>,
    streaming_text_active: bool,
    finish_streaming_text_calls: usize,
    reset_turn_state_calls: usize,
}

#[test]
fn format_runtime_stderr_prefix_includes_timestamp_and_label() {
    assert_eq!(
        format_runtime_stderr_prefix("codex", "12:59:03"),
        "codex stderr [12:59:03]>"
    );
    assert_eq!(
        format_runtime_stderr_prefix("gemini", "12:59:04"),
        "gemini stderr [12:59:04]>"
    );
    assert_eq!(
        format_runtime_stderr_prefix("claude", "12:59:05"),
        "claude stderr [12:59:05]>"
    );
}

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

    let (output, truncated) =
        join_terminal_output_reader(reader, done_rx, buffer, "stdout", Duration::from_secs(1))
            .expect("reader error should return buffered prefix as truncated output");

    assert_eq!(output, "prefix-before-error");
    assert!(truncated);
}

#[test]
fn terminal_output_reader_disconnected_reports_reader_panic() {
    let buffer = new_terminal_output_buffer();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let reader = std::thread::spawn(move || -> std::io::Result<()> {
        let _hold_sender = done_tx;
        panic!("reader exploded before completion signal");
    });

    let err =
        join_terminal_output_reader(reader, done_rx, buffer, "stdout", Duration::from_secs(1))
            .expect_err("reader panic should surface as an internal error");

    assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        err.message.contains("reader panicked"),
        "error message should name the reader panic, got: {}",
        err.message
    );
}

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

#[test]
fn truncate_child_stdout_log_line_appends_ellipsis_only_when_needed() {
    assert_eq!(truncate_child_stdout_log_line("abcdef", 4), "abcd...");
    assert_eq!(truncate_child_stdout_log_line("abc", 4), "abc");
}

#[test]
fn shared_codex_bad_json_streak_failure_detail_trips_at_threshold() {
    assert_eq!(
        shared_codex_bad_json_streak_failure_detail(
            SHARED_CODEX_MAX_CONSECUTIVE_BAD_JSON_LINES - 1,
            "warn"
        ),
        None
    );

    let detail = shared_codex_bad_json_streak_failure_detail(
        SHARED_CODEX_MAX_CONSECUTIVE_BAD_JSON_LINES,
        &"x".repeat(SHARED_CODEX_STDOUT_LOG_PREVIEW_MAX_CHARS + 5),
    )
    .expect("threshold should produce a failure detail");
    assert!(detail.contains(&format!(
        "{} consecutive non-JSON stdout lines",
        SHARED_CODEX_MAX_CONSECUTIVE_BAD_JSON_LINES
    )));
    // The user-facing detail must NOT include the raw child stdout preview.
    assert!(
        !detail.contains("xxx"),
        "raw child stdout content should not appear in user-facing failure detail"
    );
}

impl TurnRecorder for TestRecorder {
    fn note_external_session(&mut self, _session_id: &str) -> Result<()> {
        Ok(())
    }

    fn push_text(&mut self, text: &str) -> Result<()> {
        self.texts.push(text.to_owned());
        Ok(())
    }

    fn push_subagent_result(
        &mut self,
        title: &str,
        summary: &str,
        _conversation_id: Option<&str>,
        _turn_id: Option<&str>,
    ) -> Result<()> {
        self.subagent_results
            .push((title.to_owned(), summary.to_owned()));
        self.texts.push(format!("{title}\n{summary}"));
        Ok(())
    }

    fn push_approval(&mut self, title: &str, command: &str, detail: &str) -> Result<()> {
        self.approvals
            .push((title.to_owned(), command.to_owned(), detail.to_owned()));
        Ok(())
    }

    fn push_thinking(&mut self, title: &str, lines: Vec<String>) -> Result<()> {
        self.thinking.push((title.to_owned(), lines));
        Ok(())
    }

    fn push_diff(
        &mut self,
        file_path: &str,
        summary: &str,
        diff: &str,
        change_type: ChangeType,
    ) -> Result<()> {
        self.diffs.push((
            file_path.to_owned(),
            summary.to_owned(),
            diff.to_owned(),
            change_type,
        ));
        Ok(())
    }

    fn text_delta(&mut self, delta: &str) -> Result<()> {
        if !self.streaming_text_active {
            self.streaming_text_delta_start = Some(self.text_deltas.len());
            self.streaming_text_active = true;
        }
        self.text_deltas.push(delta.to_owned());
        Ok(())
    }

    fn replace_streaming_text(&mut self, text: &str) -> Result<()> {
        if let Some(start) = self.streaming_text_delta_start {
            self.text_deltas.truncate(start);
            self.text_deltas.push(text.to_owned());
            self.streaming_text_active = true;
            return Ok(());
        }

        self.texts.push(text.to_owned());
        Ok(())
    }

    fn finish_streaming_text(&mut self) -> Result<()> {
        self.streaming_text_delta_start = None;
        self.streaming_text_active = false;
        self.finish_streaming_text_calls += 1;
        Ok(())
    }

    fn reset_turn_state(&mut self) -> Result<()> {
        self.reset_turn_state_calls += 1;
        self.finish_streaming_text()
    }

    fn command_started(&mut self, _key: &str, command: &str) -> Result<()> {
        self.commands
            .push((command.to_owned(), String::new(), CommandStatus::Running));
        Ok(())
    }

    fn command_completed(
        &mut self,
        _key: &str,
        command: &str,
        output: &str,
        status: CommandStatus,
    ) -> Result<()> {
        self.commands
            .push((command.to_owned(), output.to_owned(), status));
        Ok(())
    }

    fn upsert_parallel_agents(
        &mut self,
        _key: &str,
        agents: &[ParallelAgentProgress],
    ) -> Result<()> {
        self.parallel_agents.push(agents.to_vec());
        Ok(())
    }

    fn error(&mut self, _detail: &str) -> Result<()> {
        Ok(())
    }
}

impl CodexTurnRecorder for TestRecorder {
    fn push_codex_approval(
        &mut self,
        title: &str,
        command: &str,
        detail: &str,
        approval: CodexPendingApproval,
    ) -> Result<()> {
        self.approvals
            .push((title.to_owned(), command.to_owned(), detail.to_owned()));
        self.codex_approvals.push((
            title.to_owned(),
            command.to_owned(),
            detail.to_owned(),
            approval,
        ));
        Ok(())
    }

    fn push_codex_user_input_request(
        &mut self,
        title: &str,
        detail: &str,
        questions: Vec<UserInputQuestion>,
        request: CodexPendingUserInput,
    ) -> Result<()> {
        self.codex_user_input_requests.push((
            title.to_owned(),
            detail.to_owned(),
            questions,
            request,
        ));
        Ok(())
    }

    fn push_codex_mcp_elicitation_request(
        &mut self,
        title: &str,
        detail: &str,
        request: McpElicitationRequestPayload,
        pending: CodexPendingMcpElicitation,
    ) -> Result<()> {
        self.codex_mcp_elicitation_requests.push((
            title.to_owned(),
            detail.to_owned(),
            request,
            pending,
        ));
        Ok(())
    }

    fn push_codex_app_request(
        &mut self,
        title: &str,
        detail: &str,
        method: &str,
        params: Value,
        pending: CodexPendingAppRequest,
    ) -> Result<()> {
        self.codex_app_requests.push((
            title.to_owned(),
            detail.to_owned(),
            method.to_owned(),
            params,
            pending,
        ));
        Ok(())
    }
}

#[test]
fn shared_codex_app_server_event_matches_active_turn_covers_turn_id_and_turnless_events() {
    assert!(!shared_codex_app_server_event_matches_active_turn(
        None, false, None
    ));
    assert!(shared_codex_app_server_event_matches_active_turn(
        Some("turn-1"),
        false,
        Some("turn-1")
    ));
    assert!(!shared_codex_app_server_event_matches_active_turn(
        Some("turn-1"),
        false,
        Some("turn-2")
    ));
    assert!(shared_codex_app_server_event_matches_active_turn(
        Some("turn-1"),
        true,
        None
    ));
    assert!(!shared_codex_app_server_event_matches_active_turn(
        Some("turn-1"),
        false,
        None
    ));
}

#[test]
fn clear_shared_codex_turn_recorder_state_resets_all_fields() {
    let mut recorder_state = SessionRecorderState {
        command_messages: HashMap::from([("cmd-1".to_owned(), "Running".to_owned())]),
        parallel_agents_messages: HashMap::from([("parallel-1".to_owned(), "Working".to_owned())]),
        streaming_text_message_id: Some("message-1".to_owned()),
    };

    clear_shared_codex_turn_recorder_state(&mut recorder_state);

    assert!(recorder_state.command_messages.is_empty());
    assert!(recorder_state.parallel_agents_messages.is_empty());
    assert_eq!(recorder_state.streaming_text_message_id, None);
}

#[test]
fn clear_shared_codex_turn_session_state_resets_turn_local_fields_and_preserves_thread_id() {
    let mut session_state = SharedCodexSessionState {
        pending_turn_start_request_id: Some("turn-start-1".to_owned()),
        recorder: SessionRecorderState {
            command_messages: HashMap::from([("cmd-1".to_owned(), "Running".to_owned())]),
            parallel_agents_messages: HashMap::from([(
                "parallel-1".to_owned(),
                "Working".to_owned(),
            )]),
            streaming_text_message_id: Some("message-1".to_owned()),
        },
        thread_id: Some("thread-1".to_owned()),
        turn_id: Some("turn-1".to_owned()),
        completed_turn_id: Some("turn-0".to_owned()),
        turn_started: true,
        turn_state: CodexTurnState {
            current_agent_message_id: Some("assistant-1".to_owned()),
            streamed_agent_message_text_by_item_id: HashMap::from([(
                "item-1".to_owned(),
                "hello".to_owned(),
            )]),
            streamed_agent_message_item_ids: HashSet::from(["item-1".to_owned()]),
            pending_subagent_results: vec![PendingSubagentResult {
                title: "Worker".to_owned(),
                summary: "Done".to_owned(),
                conversation_id: Some("conversation-1".to_owned()),
                turn_id: Some("turn-1".to_owned()),
            }],
            assistant_output_started: true,
            first_visible_assistant_message_id: Some("visible-1".to_owned()),
        },
    };

    clear_shared_codex_turn_session_state(&mut session_state);

    assert_eq!(session_state.pending_turn_start_request_id, None);
    assert_eq!(session_state.thread_id.as_deref(), Some("thread-1"));
    assert_eq!(session_state.turn_id, None);
    assert_eq!(session_state.completed_turn_id, None);
    assert!(!session_state.turn_started);
    assert_eq!(session_state.turn_state.current_agent_message_id, None);
    assert!(
        session_state
            .turn_state
            .streamed_agent_message_text_by_item_id
            .is_empty()
    );
    assert!(
        session_state
            .turn_state
            .streamed_agent_message_item_ids
            .is_empty()
    );
    assert!(session_state.turn_state.pending_subagent_results.is_empty());
    assert!(!session_state.turn_state.assistant_output_started);
    assert_eq!(
        session_state.turn_state.first_visible_assistant_message_id,
        None
    );
    assert!(session_state.recorder.command_messages.is_empty());
    assert!(session_state.recorder.parallel_agents_messages.is_empty());
    assert_eq!(session_state.recorder.streaming_text_message_id, None);
}

#[test]
fn clear_claude_turn_state_resets_all_fields() {
    let mut state = ClaudeTurnState {
        approval_keys_this_turn: HashSet::from(["approval-1".to_owned()]),
        parallel_agent_group_key: Some("group-1".to_owned()),
        parallel_agent_order: vec!["agent-1".to_owned()],
        parallel_agents: HashMap::from([(
            "agent-1".to_owned(),
            ParallelAgentProgress {
                detail: Some("Working".to_owned()),
                id: "agent-1".to_owned(),
                status: ParallelAgentStatus::Running,
                title: "Agent 1".to_owned(),
            },
        )]),
        permission_denied_this_turn: true,
        pending_tools: HashMap::from([(
            "tool-1".to_owned(),
            ClaudeToolUse {
                command: Some("echo hi".to_owned()),
                description: Some("Shell".to_owned()),
                file_path: Some("README.md".to_owned()),
                name: "bash".to_owned(),
                subagent_type: Some("worker".to_owned()),
            },
        )]),
        streamed_assistant_text: "partial".to_owned(),
        saw_text_delta: true,
    };

    clear_claude_turn_state(&mut state);

    assert!(state.approval_keys_this_turn.is_empty());
    assert_eq!(state.parallel_agent_group_key, None);
    assert!(state.parallel_agent_order.is_empty());
    assert!(state.parallel_agents.is_empty());
    assert!(!state.permission_denied_this_turn);
    assert!(state.pending_tools.is_empty());
    assert!(state.streamed_assistant_text.is_empty());
    assert!(!state.saw_text_delta);
}

#[test]
fn reset_claude_turn_state_clears_all_fields_and_finishes_streaming_text() {
    let mut state = ClaudeTurnState {
        approval_keys_this_turn: HashSet::from(["approval-1".to_owned()]),
        parallel_agent_group_key: Some("group-1".to_owned()),
        parallel_agent_order: vec!["agent-1".to_owned()],
        parallel_agents: HashMap::from([(
            "agent-1".to_owned(),
            ParallelAgentProgress {
                detail: Some("Working".to_owned()),
                id: "agent-1".to_owned(),
                status: ParallelAgentStatus::Running,
                title: "Agent 1".to_owned(),
            },
        )]),
        permission_denied_this_turn: true,
        pending_tools: HashMap::from([(
            "tool-1".to_owned(),
            ClaudeToolUse {
                command: Some("echo hi".to_owned()),
                description: Some("Shell".to_owned()),
                file_path: Some("README.md".to_owned()),
                name: "bash".to_owned(),
                subagent_type: Some("worker".to_owned()),
            },
        )]),
        streamed_assistant_text: "partial".to_owned(),
        saw_text_delta: true,
    };
    let mut recorder = TestRecorder {
        streaming_text_delta_start: Some(2),
        streaming_text_active: true,
        ..TestRecorder::default()
    };

    reset_claude_turn_state(&mut state, &mut recorder).unwrap();

    assert!(state.approval_keys_this_turn.is_empty());
    assert_eq!(state.parallel_agent_group_key, None);
    assert!(state.parallel_agent_order.is_empty());
    assert!(state.parallel_agents.is_empty());
    assert!(!state.permission_denied_this_turn);
    assert!(state.pending_tools.is_empty());
    assert!(state.streamed_assistant_text.is_empty());
    assert!(!state.saw_text_delta);
    assert_eq!(recorder.reset_turn_state_calls, 1);
    assert_eq!(recorder.finish_streaming_text_calls, 2);
    assert_eq!(recorder.streaming_text_delta_start, None);
    assert!(!recorder.streaming_text_active);
}

fn accept_test_connection_with_timeout(
    listener: &std::net::TcpListener,
    label: &str,
    timeout: std::time::Duration,
) -> std::net::TcpStream {
    listener
        .set_nonblocking(true)
        .expect("test listener should support nonblocking mode");
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .expect("accepted test socket should support blocking mode");
                return stream;
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "{label} timed out waiting for a connection"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(err) => panic!("{label} failed to accept a connection: {err}"),
        }
    }
}

fn accept_test_connection(listener: &std::net::TcpListener, label: &str) -> std::net::TcpStream {
    accept_test_connection_with_timeout(listener, label, std::time::Duration::from_secs(2))
}

fn join_test_server(server: std::thread::JoinHandle<()>) {
    if let Err(panic) = server.join() {
        std::panic::resume_unwind(panic);
    }
}

fn test_app_state() -> AppState {
    let persistence_path =
        std::env::temp_dir().join(format!("termal-test-{}.json", Uuid::new_v4()));

    AppState {
        default_workdir: "/tmp".to_owned(),
        persistence_path: Arc::new(persistence_path),
        orchestrator_templates_path: Arc::new(
            std::env::temp_dir().join(format!("termal-orchestrators-test-{}.json", Uuid::new_v4())),
        ),
        orchestrator_templates_lock: Arc::new(Mutex::new(())),
        review_documents_lock: Arc::new(Mutex::new(())),
        state_events: broadcast::channel(16).0,
        delta_events: broadcast::channel(16).0,
        file_events: broadcast::channel(16).0,
        file_events_revision: Arc::new(AtomicU64::new(0)),
        persist_tx: mpsc::channel().0,
        shared_codex_runtime: Arc::new(Mutex::new(None)),
        agent_readiness_cache: Arc::new(RwLock::new(fresh_agent_readiness_cache("/tmp"))),
        agent_readiness_refresh_lock: Arc::new(Mutex::new(())),
        remote_registry: test_remote_registry(),
        remote_sse_fallback_resynced_revision: Arc::new(Mutex::new(HashMap::new())),
        terminal_local_command_semaphore: Arc::new(tokio::sync::Semaphore::new(
            TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT,
        )),
        terminal_remote_command_semaphore: Arc::new(tokio::sync::Semaphore::new(
            TERMINAL_REMOTE_COMMAND_CONCURRENCY_LIMIT,
        )),
        stopping_orchestrator_ids: Arc::new(Mutex::new(HashSet::new())),
        stopping_orchestrator_session_ids: Arc::new(Mutex::new(HashMap::new())),
        inner: Arc::new(Mutex::new(StateInner::new())),
    }
}

fn test_remote_registry() -> Arc<RemoteRegistry> {
    Arc::new(
        std::thread::spawn(RemoteRegistry::new)
            .join()
            .expect("remote registry init thread panicked")
            .expect("remote registry should initialize"),
    )
}

static TEST_HOME_ENV_MUTEX: std::sync::LazyLock<std::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

#[cfg(windows)]
const TEST_HOME_ENV_KEY: &str = "USERPROFILE";
#[cfg(not(windows))]
const TEST_HOME_ENV_KEY: &str = "HOME";

struct ScopedEnvVar {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

impl ScopedEnvVar {
    fn set_path(key: &'static str, value: &FsPath) -> Self {
        let original = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value.as_os_str());
        }
        Self { key, original }
    }

    fn set(key: &'static str, value: &str) -> Self {
        let original = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, original }
    }
}

impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = self.original.as_ref() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

fn write_test_codex_threads_db(
    codex_home: &FsPath,
    rows: &[(
        &str,
        &str,
        &str,
        &str,
        &str,
        i64,
        Option<&str>,
        Option<&str>,
        i64,
    )],
) {
    fs::create_dir_all(codex_home).expect("test Codex home should be created");
    let connection =
        rusqlite::Connection::open(codex_home.join("state_5.sqlite")).expect("db should open");
    connection
        .execute_batch(
            "create table threads (
                id text primary key,
                cwd text not null,
                title text not null,
                sandbox_policy text not null,
                approval_mode text not null,
                archived integer not null,
                model text,
                reasoning_effort text,
                updated_at integer not null
            );",
        )
        .expect("threads table should be created");

    for (
        id,
        cwd,
        title,
        sandbox_policy,
        approval_mode,
        archived,
        model,
        reasoning_effort,
        updated_at,
    ) in rows
    {
        connection
            .execute(
                "insert into threads (
                    id, cwd, title, sandbox_policy, approval_mode, archived, model, reasoning_effort, updated_at
                ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    id,
                    cwd,
                    title,
                    sandbox_policy,
                    approval_mode,
                    archived,
                    model,
                    reasoning_effort,
                    updated_at
                ],
            )
            .expect("thread row should insert");
    }
}

fn test_session_id(state: &AppState, agent: Agent) -> String {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner.create_session(
        agent,
        Some("Test".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let session_id = record.session.id.clone();
    state.commit_locked(&mut inner).unwrap();
    session_id
}

fn create_test_project(state: &AppState, root_path: &FsPath, name: &str) -> String {
    state
        .create_project(CreateProjectRequest {
            name: Some(name.to_owned()),
            root_path: root_path.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap()
        .project_id
}

fn create_test_project_session(
    state: &AppState,
    agent: Agent,
    project_id: &str,
    workdir: &FsPath,
) -> String {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner.create_session(
        agent,
        Some("Test".to_owned()),
        workdir.to_string_lossy().into_owned(),
        Some(project_id.to_owned()),
        None,
    );
    let session_id = record.session.id.clone();
    state.commit_locked(&mut inner).unwrap();
    session_id
}

fn create_test_remote_project(
    state: &AppState,
    remote: &RemoteConfig,
    root_path: &str,
    name: &str,
    remote_project_id: &str,
) -> String {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    if inner.find_remote(&remote.id).is_none() {
        inner.preferences.remotes.push(remote.clone());
    }
    let project = inner.create_project(
        Some(name.to_owned()),
        root_path.to_owned(),
        remote.id.clone(),
    );
    let index = inner
        .projects
        .iter()
        .position(|candidate| candidate.id == project.id)
        .expect("remote project should exist");
    inner.projects[index].remote_project_id = Some(remote_project_id.to_owned());
    state.commit_locked(&mut inner).unwrap();
    project.id
}

fn sample_remote_orchestrator_state(
    remote_project_id: &str,
    root_path: &str,
    revision: u64,
    status: OrchestratorInstanceStatus,
) -> StateResponse {
    let draft = sample_orchestrator_template_draft();
    let template = OrchestratorTemplate {
        id: "remote-template-1".to_owned(),
        name: draft.name.clone(),
        description: draft.description.clone(),
        project_id: Some(remote_project_id.to_owned()),
        sessions: draft.sessions.clone(),
        transitions: draft.transitions.clone(),
        created_at: "2026-04-03 10:00:00".to_owned(),
        updated_at: "2026-04-03 10:00:00".to_owned(),
    };
    let remote_session_ids_by_template_session_id = draft
        .sessions
        .iter()
        .enumerate()
        .map(|(index, session)| (session.id.clone(), format!("remote-session-{}", index + 1)))
        .collect::<HashMap<_, _>>();
    let sessions = draft
        .sessions
        .iter()
        .map(|template_session| {
            let agent = template_session.agent;
            let mut session = Session {
                id: remote_session_ids_by_template_session_id[&template_session.id].clone(),
                name: template_session.name.clone(),
                emoji: agent.avatar().to_owned(),
                agent,
                workdir: root_path.to_owned(),
                project_id: Some(remote_project_id.to_owned()),
                model: template_session
                    .model
                    .clone()
                    .unwrap_or_else(|| agent.default_model().to_owned()),
                model_options: Vec::new(),
                approval_policy: None,
                reasoning_effort: None,
                sandbox_mode: None,
                cursor_mode: agent
                    .supports_cursor_mode()
                    .then_some(default_cursor_mode()),
                claude_effort: agent
                    .supports_claude_approval_mode()
                    .then_some(ClaudeEffortLevel::Default),
                claude_approval_mode: agent
                    .supports_claude_approval_mode()
                    .then_some(default_claude_approval_mode()),
                gemini_approval_mode: agent
                    .supports_gemini_approval_mode()
                    .then_some(default_gemini_approval_mode()),
                external_session_id: None,
                agent_commands_revision: 0,
                codex_thread_state: None,
                status: SessionStatus::Idle,
                preview: format!("Remote {} ready.", template_session.name),
                messages: Vec::new(),
                pending_prompts: Vec::new(),
            };
            if session.agent.supports_codex_prompt_settings() {
                session.approval_policy = Some(default_codex_approval_policy());
                session.reasoning_effort = Some(default_codex_reasoning_effort());
                session.sandbox_mode = Some(default_codex_sandbox_mode());
            }
            session
        })
        .collect::<Vec<_>>();
    let pending_transitions = if status == OrchestratorInstanceStatus::Stopped {
        Vec::new()
    } else {
        let transition = draft
            .transitions
            .first()
            .expect("sample draft should include a transition");
        vec![PendingTransition {
            id: "remote-pending-1".to_owned(),
            transition_id: transition.id.clone(),
            source_session_id: remote_session_ids_by_template_session_id
                [&transition.from_session_id]
                .clone(),
            destination_session_id: remote_session_ids_by_template_session_id
                [&transition.to_session_id]
                .clone(),
            completion_revision: 7,
            rendered_prompt: "Review the implementation.".to_owned(),
            created_at: "2026-04-03 10:05:00".to_owned(),
        }]
    };
    StateResponse {
        revision,
        codex: CodexState::default(),
        agent_readiness: Vec::new(),
        preferences: AppPreferences::default(),
        projects: Vec::new(),
        workspaces: Vec::new(),
        orchestrators: vec![OrchestratorInstance {
            id: "remote-orchestrator-1".to_owned(),
            remote_id: None,
            remote_orchestrator_id: None,
            template_id: template.id.clone(),
            project_id: remote_project_id.to_owned(),
            template_snapshot: template,
            status,
            session_instances: draft
                .sessions
                .iter()
                .map(|template_session| OrchestratorSessionInstance {
                    template_session_id: template_session.id.clone(),
                    session_id: remote_session_ids_by_template_session_id[&template_session.id]
                        .clone(),
                    last_completion_revision: None,
                    last_delivered_completion_revision: None,
                })
                .collect(),
            pending_transitions,
            created_at: "2026-04-03 10:00:00".to_owned(),
            error_message: None,
            completed_at: (status == OrchestratorInstanceStatus::Stopped)
                .then_some("2026-04-03 10:15:00".to_owned()),
            stop_in_progress: false,
            active_session_ids_during_stop: None,
            stopped_session_ids_during_stop: Vec::new(),
        }],
        sessions,
    }
}

fn run_git_test_command(repo_root: &FsPath, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("failed to run git {:?}: {err}", args));

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        panic!(
            "git {:?} failed with status {}.\nstdout: {}\nstderr: {}",
            args, output.status, stdout, stderr
        );
    }
}

fn run_git_test_command_output(repo_root: &FsPath, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("failed to run git {:?}: {err}", args));

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        panic!(
            "git {:?} failed with status {}.\nstdout: {}\nstderr: {}",
            args, output.status, stdout, stderr
        );
    }

    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn test_exit_success_child() -> Child {
    if cfg!(windows) {
        Command::new("cmd").args(["/C", "exit 0"]).spawn().unwrap()
    } else {
        Command::new("sh").arg("-c").arg("exit 0").spawn().unwrap()
    }
}

fn test_sleep_child() -> Child {
    if cfg!(windows) {
        Command::new("cmd")
            .args(["/C", "ping -n 6 127.0.0.1 >NUL"])
            .spawn()
            .unwrap()
    } else {
        Command::new("sh").arg("-c").arg("sleep 5").spawn().unwrap()
    }
}

struct TestKillChildProcessFailureGuard;

impl Drop for TestKillChildProcessFailureGuard {
    fn drop(&mut self) {
        set_test_kill_child_process_failure(None, None);
    }
}

fn force_test_kill_child_process_failure(
    process: &Arc<SharedChild>,
    label: &str,
) -> TestKillChildProcessFailureGuard {
    set_test_kill_child_process_failure(Some(label), Some(process));
    TestKillChildProcessFailureGuard
}

fn test_codex_runtime_handle(
    runtime_id: &str,
) -> (CodexRuntimeHandle, mpsc::Receiver<CodexRuntimeCommand>) {
    let child = test_exit_success_child();
    let (input_tx, input_rx) = mpsc::channel();

    (
        CodexRuntimeHandle {
            runtime_id: runtime_id.to_owned(),
            input_tx,
            process: Arc::new(SharedChild::new(child).unwrap()),
            shared_session: None,
        },
        input_rx,
    )
}

fn test_claude_runtime_handle(
    runtime_id: &str,
) -> (ClaudeRuntimeHandle, mpsc::Receiver<ClaudeRuntimeCommand>) {
    let child = test_exit_success_child();
    let (input_tx, input_rx) = mpsc::channel();

    (
        ClaudeRuntimeHandle {
            runtime_id: runtime_id.to_owned(),
            input_tx,
            process: Arc::new(SharedChild::new(child).unwrap()),
        },
        input_rx,
    )
}

fn test_acp_runtime_handle(
    agent: AcpAgent,
    runtime_id: &str,
) -> (AcpRuntimeHandle, mpsc::Receiver<AcpRuntimeCommand>) {
    let child = test_exit_success_child();
    let (input_tx, input_rx) = mpsc::channel();

    (
        AcpRuntimeHandle {
            agent,
            runtime_id: runtime_id.to_owned(),
            input_tx,
            process: Arc::new(SharedChild::new(child).unwrap()),
        },
        input_rx,
    )
}

#[derive(Clone, Default)]
struct SharedBufferWriter {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl SharedBufferWriter {
    fn contents(&self) -> String {
        String::from_utf8(
            self.buffer
                .lock()
                .expect("shared writer mutex poisoned")
                .clone(),
        )
        .expect("shared writer buffer should stay UTF-8")
    }
}

impl std::io::Write for SharedBufferWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buffer
            .lock()
            .expect("shared writer mutex poisoned")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn shared_codex_watched_writer_clears_activity_after_successful_write() {
    let activity: SharedCodexStdinActivityState = Arc::new(Mutex::new(None));
    let mut writer = SharedCodexWatchedWriter::new(SharedBufferWriter::default(), activity.clone());

    write_codex_json_rpc_message(&mut writer, &json_rpc_notification_message("initialized"))
        .expect("tracked shared Codex writer should write successfully");

    assert!(
        activity
            .lock()
            .expect("shared Codex stdin activity mutex poisoned")
            .is_none()
    );
}

fn take_pending_acp_request(
    pending_requests: &AcpPendingRequestMap,
    timeout: Duration,
) -> (
    String,
    std::sync::mpsc::Sender<std::result::Result<Value, AcpResponseError>>,
) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(request) = {
            let mut locked = pending_requests
                .lock()
                .expect("ACP pending requests mutex poisoned");
            let request_id = locked.keys().next().cloned();
            request_id.and_then(|request_id| {
                locked
                    .remove(&request_id)
                    .map(|sender| (request_id, sender))
            })
        } {
            return request;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "ACP request should arrive before timeout"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn take_pending_codex_request(
    pending_requests: &CodexPendingRequestMap,
    timeout: Duration,
) -> (
    String,
    std::sync::mpsc::Sender<std::result::Result<Value, CodexResponseError>>,
) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(request) = {
            let mut locked = pending_requests
                .lock()
                .expect("Codex pending requests mutex poisoned");
            let request_id = locked.keys().next().cloned();
            request_id.and_then(|request_id| {
                locked
                    .remove(&request_id)
                    .map(|sender| (request_id, sender))
            })
        } {
            return request;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Codex request should arrive before timeout"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn cursor_permission_request(request_id: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "method": "session/request_permission",
        "params": {
            "toolName": "edit_file",
            "description": "Edit src/main.rs",
            "options": [
                { "optionId": "allow-once" },
                { "optionId": "allow-always" },
                { "optionId": "reject-once" }
            ]
        }
    })
}

// Tests that Claude task tool use updates parallel agent progress.
#[test]
fn claude_task_tool_use_updates_parallel_agent_progress() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "task-1",
                        "name": "Task",
                        "input": {
                            "description": "Rust code review",
                            "subagent_type": "general-purpose"
                        }
                    },
                    {
                        "type": "tool_use",
                        "id": "task-2",
                        "name": "Task",
                        "input": {
                            "description": "Architecture code review",
                            "subagent_type": "general-purpose"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let latest = recorder
        .parallel_agents
        .last()
        .expect("parallel agents update should be recorded");
    assert_eq!(latest.len(), 2);
    assert_eq!(latest[0].title, "Rust code review");
    assert_eq!(latest[0].detail.as_deref(), Some("Initializing..."));
    assert_eq!(latest[0].status, ParallelAgentStatus::Initializing);
    assert_eq!(latest[1].title, "Architecture code review");
    assert_eq!(latest[1].status, ParallelAgentStatus::Initializing);
}

// Tests that Claude task tool result updates parallel agents and records subagent result.
#[test]
fn claude_task_tool_result_updates_parallel_agents_and_records_subagent_result() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "task-1",
                        "name": "Task",
                        "input": {
                            "description": "Rust code review",
                            "subagent_type": "general-purpose"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let detail = "Reviewer found a batching bug in location smoothing.\nRead src/state.rs for the stale preview path.";
    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "task-1",
                        "content": detail
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let latest = recorder
        .parallel_agents
        .last()
        .expect("completed parallel agent update should be recorded");
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].title, "Rust code review");
    assert_eq!(latest[0].status, ParallelAgentStatus::Completed);
    assert_eq!(
        latest[0].detail.as_deref(),
        Some("Reviewer found a batching bug in location smoothing.")
    );
    assert_eq!(
        recorder.subagent_results,
        vec![("Rust code review".to_owned(), detail.to_owned())]
    );
}

// Tests that Claude task tool error records full failure detail.
#[test]
fn claude_task_tool_error_records_full_failure_detail() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "task-1",
                        "name": "Task",
                        "input": {
                            "description": "Rust code review",
                            "subagent_type": "general-purpose"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let detail = "Reviewer failed to parse the diff.\nStack trace line 1\nStack trace line 2";
    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "task-1",
                        "is_error": true,
                        "content": detail
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let latest = recorder
        .parallel_agents
        .last()
        .expect("errored parallel agent update should be recorded");
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].title, "Rust code review");
    assert_eq!(latest[0].status, ParallelAgentStatus::Error);
    assert_eq!(
        latest[0].detail.as_deref(),
        Some("Reviewer failed to parse the diff.")
    );
    assert_eq!(
        recorder.subagent_results,
        vec![("Rust code review".to_owned(), detail.to_owned())]
    );
}

// Tests that Claude task tool error without detail records fallback failure message.
#[test]
fn claude_task_tool_error_without_detail_records_fallback_failure_message() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "task-1",
                        "name": "Task",
                        "input": {
                            "description": "Rust code review",
                            "subagent_type": "general-purpose"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "task-1",
                        "is_error": true,
                        "content": ""
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let latest = recorder
        .parallel_agents
        .last()
        .expect("errored parallel agent update should be recorded");
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].status, ParallelAgentStatus::Error);
    assert_eq!(latest[0].detail.as_deref(), Some("Task failed."));
    assert_eq!(
        recorder.subagent_results,
        vec![("Rust code review".to_owned(), "Task failed.".to_owned())]
    );
}

// Tests that Claude streamed text appends missing final suffix after message stop.
#[test]
fn claude_streamed_text_appends_missing_final_suffix_after_message_stop() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn_state = ClaudeTurnState::default();
    let mut external_session_id = None;

    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "Hello"
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "message_stop"
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "text",
                        "text": "Hello there."
                    }
                ]
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "result",
            "is_error": false
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Claude session should exist");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Hello there."
    ));
}

// Tests that Claude streamed text skips duplicate final text after message stop.
#[test]
fn claude_streamed_text_skips_duplicate_final_text_after_message_stop() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn_state = ClaudeTurnState::default();
    let mut external_session_id = None;

    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "Hello there."
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "message_stop"
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "text",
                        "text": "Hello there."
                    }
                ]
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "result",
            "is_error": false
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Claude session should exist");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Hello there."
    ));
}

// Tests that Claude streamed text replaces divergent final text.
#[test]
fn claude_streamed_text_replaces_divergent_final_text() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn_state = ClaudeTurnState::default();
    let mut external_session_id = None;

    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "Draft answer."
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "text",
                        "text": "Final answer."
                    }
                ]
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    handle_claude_event(
        &json!({
            "type": "result",
            "is_error": false
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Claude session should exist");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Final answer."
    ));
}

// Tests that Claude tool use after streamed text starts followup in new message.
#[test]
fn claude_tool_use_after_streamed_text_starts_followup_in_new_message() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn_state = ClaudeTurnState::default();
    let mut external_session_id = None;

    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "Hello"
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "message_stop"
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "bash-1",
                        "name": "Bash",
                        "input": {
                            "command": "pwd"
                        }
                    }
                ]
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "World"
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "result",
            "is_error": false
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Claude session should exist");

    assert_eq!(session.messages.len(), 3);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Hello"
    ));
    assert!(matches!(
        session.messages.get(1),
        Some(Message::Command {
            command,
            output,
            status,
            ..
        }) if command == "pwd" && output.is_empty() && *status == CommandStatus::Running
    ));
    assert!(matches!(
        session.messages.get(2),
        Some(Message::Text { text, .. }) if text == "World"
    ));
}

// Tests that Claude result clears pending tools and ignores late tool results.
#[test]
fn claude_result_clears_pending_tools_and_ignores_late_tool_results() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "bash-1",
                        "name": "Bash",
                        "input": {
                            "command": "pwd"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "result",
            "is_error": false
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    assert!(turn_state.pending_tools.is_empty());

    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "bash-1",
                        "content": "/tmp/late"
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(
        recorder.commands,
        vec![("pwd".to_owned(), String::new(), CommandStatus::Running)]
    );
}

// Tests that Claude result resets recorder command keys between turns.
#[test]
fn claude_result_resets_recorder_command_keys_between_turns() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn_state = ClaudeTurnState::default();
    let mut external_session_id = None;

    for (command, output) in [("pwd", "/tmp/one"), ("git status", "working tree clean")] {
        handle_claude_event(
            &json!({
                "type": "assistant",
                "message": {
                    "content": [
                        {
                            "type": "tool_use",
                            "id": "bash-1",
                            "name": "Bash",
                            "input": {
                                "command": command
                            }
                        }
                    ]
                }
            }),
            &mut external_session_id,
            &mut turn_state,
            &mut recorder,
        )
        .unwrap();
        handle_claude_event(
            &json!({
                "type": "user",
                "message": {
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": "bash-1",
                            "content": output
                        }
                    ]
                }
            }),
            &mut external_session_id,
            &mut turn_state,
            &mut recorder,
        )
        .unwrap();
        handle_claude_event(
            &json!({
                "type": "result",
                "is_error": false
            }),
            &mut external_session_id,
            &mut turn_state,
            &mut recorder,
        )
        .unwrap();
    }

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Claude session should exist");
    let commands = session
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Command {
                command,
                output,
                status,
                ..
            } => Some((command.clone(), output.clone(), *status)),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        commands,
        vec![
            (
                "pwd".to_owned(),
                "/tmp/one".to_owned(),
                CommandStatus::Success
            ),
            (
                "git status".to_owned(),
                "working tree clean".to_owned(),
                CommandStatus::Success
            ),
        ]
    );
}

// Tests that ACP JSON RPC request without timeout waits for late response.
#[test]
fn acp_json_rpc_request_without_timeout_waits_for_late_response() {
    let pending_requests: AcpPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let (result_tx, result_rx) = mpsc::channel();
    let request_pending_requests = pending_requests.clone();

    std::thread::spawn(move || {
        let mut writer = Vec::new();
        let result = send_acp_json_rpc_request_without_timeout(
            &mut writer,
            &request_pending_requests,
            "session/prompt",
            json!({
                "sessionId": "cursor-session-1",
                "prompt": [],
            }),
            AcpAgent::Cursor,
        )
        .expect("prompt request should resolve once a response arrives");
        result_tx
            .send((
                String::from_utf8(writer).expect("request payload should be UTF-8"),
                result,
            ))
            .unwrap();
    });

    let (request_id, sender) = take_pending_acp_request(&pending_requests, Duration::from_secs(1));

    sender.send(Ok(json!({ "ok": true }))).unwrap();

    let (written, result) = result_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("late ACP response should unblock the prompt request");
    assert!(written.contains("\"method\":\"session/prompt\""));
    assert!(written.contains(&format!("\"id\":\"{request_id}\"")));
    assert_eq!(result, json!({ "ok": true }));
    assert!(
        pending_requests
            .lock()
            .expect("ACP pending requests mutex poisoned")
            .is_empty()
    );
}

// Tests that Codex JSON RPC request without timeout waits for late response.
#[test]
fn codex_json_rpc_request_without_timeout_waits_for_late_response() {
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let (result_tx, result_rx) = mpsc::channel();
    let request_pending_requests = pending_requests.clone();

    std::thread::spawn(move || {
        let mut writer = Vec::new();
        let result = send_codex_json_rpc_request_without_timeout(
            &mut writer,
            &request_pending_requests,
            "turn/start",
            json!({
                "threadId": "thread-1",
            }),
        )
        .expect("Codex request should resolve once a response arrives");
        result_tx
            .send((
                String::from_utf8(writer).expect("request payload should be UTF-8"),
                result,
            ))
            .unwrap();
    });

    let (request_id, sender) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));

    sender.send(Ok(json!({ "ok": true }))).unwrap();

    let (written, result) = result_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("late Codex response should unblock the request");
    assert!(written.contains("\"method\":\"turn/start\""));
    assert!(written.contains(&format!("\"id\":\"{request_id}\"")));
    assert_eq!(result, json!({ "ok": true }));
    assert!(
        pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned")
            .is_empty()
    );
}

// Tests that Codex JSON RPC request preserves JSON-RPC errors.
#[test]
fn codex_json_rpc_request_without_timeout_preserves_json_rpc_errors() {
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let (result_tx, result_rx) = mpsc::channel();
    let request_pending_requests = pending_requests.clone();

    std::thread::spawn(move || {
        let mut writer = Vec::new();
        let result = send_codex_json_rpc_request_without_timeout(
            &mut writer,
            &request_pending_requests,
            "turn/start",
            json!({
                "threadId": "thread-1",
            }),
        );
        result_tx.send(result).unwrap();
    });

    let (request_id, sender) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));

    assert!(!request_id.is_empty());
    sender
        .send(Err(CodexResponseError::JsonRpc(
            "thread/start rejected the request".to_owned(),
        )))
        .unwrap();

    assert_eq!(
        result_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        Err(CodexResponseError::JsonRpc(
            "thread/start rejected the request".to_owned(),
        ))
    );
    assert!(
        pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned")
            .is_empty()
    );
}

// Tests that waiting for a Codex JSON-RPC response times out and clears the pending request.
#[test]
fn codex_json_rpc_response_wait_timeout_clears_pending_request() {
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = Vec::new();

    let pending_request = start_codex_json_rpc_request(
        &mut writer,
        &pending_requests,
        "turn/start",
        json!({
            "threadId": "thread-1",
        }),
    )
    .expect("Codex request should be queued");

    let result = wait_for_codex_json_rpc_response(
        &pending_requests,
        pending_request,
        "turn/start",
        Some(Duration::from_millis(10)),
    );

    assert!(matches!(
        result,
        Err(CodexResponseError::Timeout(detail))
            if detail.contains("timed out waiting for Codex app-server response to `turn/start`")
    ));
    assert!(
        pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned")
            .is_empty()
    );
}

// Tests that ACP prompt command keeps writer loop responsive while waiting for response.
#[test]
fn acp_prompt_command_keeps_writer_loop_responsive_while_waiting_for_response() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Prompt Loop".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Ask),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: Some("cursor-session-1".to_owned()),
        is_loading_history: false,
        supports_session_load: Some(true),
    }));
    let writer = SharedBufferWriter::default();
    let thread_writer = writer.clone();
    let thread_pending_requests = pending_requests.clone();
    let thread_runtime_state = runtime_state.clone();
    let thread_state = state.clone();
    let thread_session_id = created.session_id.clone();
    let runtime_token = RuntimeToken::Acp("cursor-runtime-1".to_owned());
    let (input_tx, input_rx) = mpsc::channel();

    let writer_thread = std::thread::spawn(move || {
        let mut stdin = thread_writer;
        while let Ok(command) = input_rx.recv_timeout(Duration::from_millis(250)) {
            match command {
                AcpRuntimeCommand::Prompt(prompt) => handle_acp_prompt_command(
                    &mut stdin,
                    &thread_pending_requests,
                    &thread_state,
                    &thread_session_id,
                    &thread_runtime_state,
                    &runtime_token,
                    AcpAgent::Cursor,
                    prompt,
                )
                .unwrap(),
                AcpRuntimeCommand::JsonRpcMessage(message) => {
                    write_acp_json_rpc_message(&mut stdin, &message, AcpAgent::Cursor).unwrap();
                }
                AcpRuntimeCommand::RefreshSessionConfig { .. } => {
                    panic!("unexpected config refresh in prompt loop test");
                }
            }
        }
    });

    input_tx
        .send(AcpRuntimeCommand::Prompt(AcpPromptCommand {
            cwd: "/tmp".to_owned(),
            cursor_mode: Some(CursorMode::Ask),
            model: "auto".to_owned(),
            prompt: "review-local".to_owned(),
            resume_session_id: Some("cursor-session-1".to_owned()),
        }))
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        if pending_requests
            .lock()
            .expect("ACP pending requests mutex poisoned")
            .len()
            == 1
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "prompt request should stay pending while waiting for a response"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    input_tx
        .send(AcpRuntimeCommand::JsonRpcMessage(
            json_rpc_result_response_message(
                "approval-1".to_owned(),
                json!({
                    "outcome": {
                        "outcome": "selected",
                        "optionId": "allow-once",
                    }
                }),
            ),
        ))
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let written = writer.contents();
        if written.contains("\"method\":\"session/prompt\"")
            && written.contains("\"id\":\"approval-1\"")
            && written.contains("\"jsonrpc\":\"2.0\"")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "writer loop should remain able to write approval responses while prompt is pending"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let sender = {
        let mut locked = pending_requests
            .lock()
            .expect("ACP pending requests mutex poisoned");
        let request_id = locked
            .keys()
            .next()
            .cloned()
            .expect("prompt request id should exist");
        locked
            .remove(&request_id)
            .expect("prompt request sender should still be pending")
    };
    sender.send(Ok(json!({ "ok": true }))).unwrap();

    drop(input_tx);
    writer_thread.join().unwrap();
}

// Tests that fail pending ACP requests releases waiters.
#[test]
fn fail_pending_acp_requests_releases_waiters() {
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = mpsc::channel::<std::result::Result<Value, AcpResponseError>>();

    pending_requests
        .lock()
        .expect("ACP pending requests mutex poisoned")
        .insert("req-1".to_owned(), tx);

    fail_pending_acp_requests(&pending_requests, "Cursor ACP runtime exited.");

    assert!(
        pending_requests
            .lock()
            .expect("ACP pending requests mutex poisoned")
            .is_empty()
    );
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        Err(AcpResponseError::Transport(
            "Cursor ACP runtime exited.".to_owned()
        ))
    );
}

// Tests that fail pending Codex requests releases waiters.
#[test]
fn fail_pending_codex_requests_releases_waiters() {
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = mpsc::channel::<std::result::Result<Value, CodexResponseError>>();

    pending_requests
        .lock()
        .expect("Codex pending requests mutex poisoned")
        .insert("req-1".to_owned(), tx);

    fail_pending_codex_requests(
        &pending_requests,
        "shared Codex app-server exited while waiting for a pending response",
    );

    assert!(
        pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned")
            .is_empty()
    );
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        Err(CodexResponseError::Transport(
            "shared Codex app-server exited while waiting for a pending response".to_owned()
        ))
    );
}

fn test_shared_codex_runtime(
    runtime_id: &str,
) -> (
    SharedCodexRuntime,
    mpsc::Receiver<CodexRuntimeCommand>,
    Arc<SharedChild>,
) {
    let child = test_exit_success_child();
    let process = Arc::new(SharedChild::new(child).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = SharedCodexRuntime {
        runtime_id: runtime_id.to_owned(),
        input_tx,
        process: process.clone(),
        sessions: SharedCodexSessions::new(),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    (runtime, input_rx, process)
}

async fn request_json<T: for<'de> Deserialize<'de>>(
    app: &Router,
    request: Request<Body>,
) -> (StatusCode, T) {
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("request should complete");
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body should read");
    let parsed = serde_json::from_slice(&body).expect("response body should be valid JSON");
    (status, parsed)
}

async fn request_response(app: &Router, request: Request<Body>) -> axum::response::Response {
    app.clone()
        .oneshot(request)
        .await
        .expect("request should complete")
}
async fn next_sse_event<S>(stream: &mut std::pin::Pin<Box<S>>) -> String
where
    S: futures_core::Stream<Item = Result<axum::body::Bytes, axum::Error>>,
{
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut event = String::new();
        loop {
            let chunk = std::future::poll_fn(|cx| stream.as_mut().poll_next(cx))
                .await
                .expect("SSE chunk should arrive")
                .expect("SSE chunk should stream cleanly");
            event.push_str(
                std::str::from_utf8(chunk.as_ref()).expect("SSE chunk should be valid UTF-8"),
            );
            if event.contains("\n\n") || event.contains("\r\n\r\n") {
                return event;
            }
        }
    })
    .await
    .expect("SSE event should arrive before timeout")
}
fn parse_sse_event(raw: &str) -> (String, String) {
    let mut event_name = None;
    let mut data_lines = Vec::new();
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(value) = line.strip_prefix("event: ") {
            event_name = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("data: ") {
            data_lines.push(value.to_owned());
        }
    }
    (
        event_name.expect("SSE event should include a name"),
        data_lines.join("\n"),
    )
}
// Tests that wait for shared child exit timeout returns status for completed process.
#[test]
fn wait_for_shared_child_exit_timeout_returns_status_for_completed_process() {
    let child = test_exit_success_child();
    let process = Arc::new(SharedChild::new(child).unwrap());

    let status = wait_for_shared_child_exit_timeout(&process, Duration::from_secs(1), "test child")
        .unwrap()
        .expect("completed process should return a status");

    assert!(status.success());
}

// Tests that wait for shared child exit timeout returns none for running process.
#[test]
fn wait_for_shared_child_exit_timeout_returns_none_for_running_process() {
    let child = test_sleep_child();
    let process = Arc::new(SharedChild::new(child).unwrap());

    let status =
        wait_for_shared_child_exit_timeout(&process, Duration::from_millis(10), "test child")
            .unwrap();

    assert!(status.is_none());
    process.kill().unwrap();
    process.wait().unwrap();
}

// Tests that shutdown REPL Codex process forces running process after timeout.
#[test]
fn shutdown_repl_codex_process_forces_running_process_after_timeout() {
    let child = test_sleep_child();
    let process = Arc::new(SharedChild::new(child).unwrap());

    let (status, forced_shutdown) = shutdown_repl_codex_process(&process).unwrap();

    assert!(forced_shutdown);
    assert!(!status.success());
}

// Tests that reads Claude agent commands from markdown files.
#[test]
fn reads_claude_agent_commands_from_markdown_files() {
    let root = std::env::temp_dir().join(format!("termal-agent-commands-{}", Uuid::new_v4()));
    let commands_dir = root.join(".claude").join("commands");

    fs::create_dir_all(commands_dir.join("nested")).unwrap();
    fs::write(
        commands_dir.join("review-local.md"),
        "Review local changes.

## Step 1
Inspect diffs.
",
    )
    .unwrap();
    fs::write(
        commands_dir.join("fix-bug.md"),
        "
Fix a bug from docs/bugs.md by number.

$ARGUMENTS
",
    )
    .unwrap();
    fs::write(commands_dir.join("notes.txt"), "ignore").unwrap();
    fs::write(commands_dir.join("nested").join("ignored.md"), "ignore").unwrap();

    let commands = read_claude_agent_commands(&root).unwrap();

    assert_eq!(
        commands,
        vec![
            AgentCommand {
                kind: AgentCommandKind::PromptTemplate,
                name: "fix-bug".to_owned(),
                description: "Fix a bug from docs/bugs.md by number.".to_owned(),
                content: "
Fix a bug from docs/bugs.md by number.

$ARGUMENTS
"
                .to_owned(),
                source: ".claude/commands/fix-bug.md".to_owned(),
                argument_hint: None,
            },
            AgentCommand {
                kind: AgentCommandKind::PromptTemplate,
                name: "review-local".to_owned(),
                description: "Review local changes.".to_owned(),
                content: "Review local changes.

## Step 1
Inspect diffs.
"
                .to_owned(),
                source: ".claude/commands/review-local.md".to_owned(),
                argument_hint: None,
            },
        ]
    );

    fs::remove_dir_all(root).unwrap();
}

// Tests that returns empty agent commands when commands directory is missing.
#[test]
fn returns_empty_agent_commands_when_commands_directory_is_missing() {
    let root =
        std::env::temp_dir().join(format!("termal-agent-commands-missing-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();

    let commands = read_claude_agent_commands(&root).unwrap();
    assert!(commands.is_empty());

    fs::remove_dir_all(root).unwrap();
}

// Tests that returns agent commands for non Claude sessions.
#[test]
fn returns_agent_commands_for_non_claude_sessions() {
    let root = std::env::temp_dir().join(format!("termal-agent-commands-codex-{}", Uuid::new_v4()));
    let commands_dir = root.join(".claude").join("commands");
    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("review-local.md"),
        "Review local changes.

Use the active agent's tools.
",
    )
    .unwrap();

    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Session".to_owned()),
            workdir: Some(root.to_string_lossy().into_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let response = state.list_agent_commands(&created.session_id).unwrap();
    assert_eq!(response.commands.len(), 1);
    assert_eq!(response.commands[0].name, "review-local");
    assert_eq!(response.commands[0].description, "Review local changes.");
    assert_eq!(response.commands[0].kind, AgentCommandKind::PromptTemplate);

    fs::remove_dir_all(root).unwrap();
}

// Tests that extracts Claude native agent commands from initialize response.
#[test]
fn extracts_claude_native_agent_commands_from_initialize_response() {
    let message = json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "response": {
                "commands": [
                    {
                        "name": "review",
                        "description": "Review the current changes. (bundled)",
                        "argumentHint": ""
                    },
                    {
                        "name": "review-local",
                        "description": "Review local changes. (project)",
                        "argumentHint": "[scope]"
                    }
                ]
            }
        }
    });

    assert_eq!(
        claude_agent_commands(&message),
        Some(vec![
            AgentCommand {
                kind: AgentCommandKind::NativeSlash,
                name: "review".to_owned(),
                description: "Review the current changes.".to_owned(),
                content: "/review".to_owned(),
                source: "Claude bundled command".to_owned(),
                argument_hint: None,
            },
            AgentCommand {
                kind: AgentCommandKind::NativeSlash,
                name: "review-local".to_owned(),
                description: "Review local changes.".to_owned(),
                content: "/review-local".to_owned(),
                source: "Claude project command".to_owned(),
                argument_hint: Some("[scope]".to_owned()),
            },
        ])
    );
}

// Tests that extracts Claude native agent commands filters empty names and normalizes user suffix.
#[test]
fn extracts_claude_native_agent_commands_filters_empty_names_and_normalizes_user_suffix() {
    let message = json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "response": {
                "commands": [
                    {
                        "name": "   ",
                        "description": "Should be filtered."
                    },
                    {
                        "name": "release-notes",
                        "description": "Draft release notes. (user)"
                    }
                ]
            }
        }
    });

    assert_eq!(
        claude_agent_commands(&message),
        Some(vec![AgentCommand {
            kind: AgentCommandKind::NativeSlash,
            name: "release-notes".to_owned(),
            description: "Draft release notes.".to_owned(),
            content: "/release-notes".to_owned(),
            source: "Claude user command".to_owned(),
            argument_hint: None,
        }])
    );
}

// Tests that extracts Claude native agent commands returns none for empty command list.
#[test]
fn extracts_claude_native_agent_commands_returns_none_for_empty_command_list() {
    let message = json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "response": {
                "commands": []
            }
        }
    });

    assert_eq!(claude_agent_commands(&message), None);
}

// Tests that returns cached Claude native commands alongside template fallbacks.
#[test]
fn returns_cached_claude_native_commands_alongside_template_fallbacks() {
    let root = std::env::temp_dir().join(format!(
        "termal-agent-commands-claude-native-{}",
        Uuid::new_v4()
    ));
    let commands_dir = root.join(".claude").join("commands");
    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("review-local.md"),
        "Review local changes from the filesystem template.",
    )
    .unwrap();
    fs::write(
        commands_dir.join("fix-bug.md"),
        "Fix a bug from docs/bugs.md by number.\n\n$ARGUMENTS\n",
    )
    .unwrap();

    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude Session".to_owned()),
            workdir: Some(root.to_string_lossy().into_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    state
        .sync_session_agent_commands(
            &created.session_id,
            vec![
                AgentCommand {
                    kind: AgentCommandKind::NativeSlash,
                    name: "review".to_owned(),
                    description: "Review the current changes.".to_owned(),
                    content: "/review".to_owned(),
                    source: "Claude bundled command".to_owned(),
                    argument_hint: None,
                },
                AgentCommand {
                    kind: AgentCommandKind::NativeSlash,
                    name: "review-local".to_owned(),
                    description: "Review local changes.".to_owned(),
                    content: "/review-local".to_owned(),
                    source: "Claude project command".to_owned(),
                    argument_hint: Some("[scope]".to_owned()),
                },
            ],
        )
        .unwrap();

    let response = state.list_agent_commands(&created.session_id).unwrap();

    assert_eq!(
        response
            .commands
            .iter()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>(),
        vec!["fix-bug", "review", "review-local"]
    );
    assert_eq!(response.commands[0].kind, AgentCommandKind::PromptTemplate);
    assert_eq!(response.commands[1].kind, AgentCommandKind::NativeSlash);
    assert_eq!(response.commands[2].kind, AgentCommandKind::NativeSlash);
    assert_eq!(
        response.commands[2].argument_hint.as_deref(),
        Some("[scope]")
    );
    assert_eq!(response.commands[2].source, "Claude project command");

    drop(response);
    drop(created);
    drop(state);
    let _ = fs::remove_dir_all(&root);
}

// Tests that sync session agent commands bumps visible session command revision.
#[test]
fn sync_session_agent_commands_bumps_visible_session_command_revision() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude Session".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let starting_revision = created.state.revision;
    let starting_session_revision = created
        .state
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("created Claude session should exist")
        .agent_commands_revision;

    state
        .sync_session_agent_commands(
            &created.session_id,
            vec![AgentCommand {
                kind: AgentCommandKind::NativeSlash,
                name: "review".to_owned(),
                description: "Review the current changes.".to_owned(),
                content: "/review".to_owned(),
                source: "Claude bundled command".to_owned(),
                argument_hint: None,
            }],
        )
        .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Claude session should exist");
    assert!(snapshot.revision > starting_revision);
    assert_eq!(
        session.agent_commands_revision,
        starting_session_revision.saturating_add(1)
    );
}

// Tests that returns not found for missing agent command session.
#[test]
fn returns_not_found_for_missing_agent_command_session() {
    let state = test_app_state();
    let error = state.list_agent_commands("missing-session").unwrap_err();

    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert_eq!(error.message, "session not found");
}

// Tests that instruction search returns all roots for a phrase.
#[test]
fn instruction_search_returns_all_roots_for_a_phrase() {
    let root = std::env::temp_dir().join(format!("termal-instruction-search-{}", Uuid::new_v4()));
    let docs_dir = root.join("docs");
    fs::create_dir_all(&docs_dir).unwrap();
    fs::write(
        root.join("AGENTS.md"),
        "See docs/backend.md for service rules.\n",
    )
    .unwrap();
    fs::write(
        root.join("CLAUDE.md"),
        "Use docs/backend.md for implementation guidance.\n",
    )
    .unwrap();
    fs::write(
        docs_dir.join("backend.md"),
        "# Backend\n\nPrefer dependency injection when module boundaries shift.\n",
    )
    .unwrap();

    let response = search_instruction_phrase(&root, "dependency injection").unwrap();

    assert_eq!(response.matches.len(), 1);
    let matched = &response.matches[0];
    assert!(
        matched.path.ends_with("docs\\backend.md") || matched.path.ends_with("docs/backend.md")
    );
    assert_eq!(
        matched.text,
        "Prefer dependency injection when module boundaries shift."
    );
    assert_eq!(matched.root_paths.len(), 2);
    assert_eq!(
        matched
            .root_paths
            .iter()
            .map(|root_path| root_path.root_path.clone())
            .collect::<Vec<_>>(),
        vec![
            normalize_path_best_effort(&root.join("AGENTS.md"))
                .to_string_lossy()
                .into_owned(),
            normalize_path_best_effort(&root.join("CLAUDE.md"))
                .to_string_lossy()
                .into_owned(),
        ]
    );
    assert!(
        matched
            .root_paths
            .iter()
            .all(|root_path| root_path.steps.len() == 1
                && root_path.steps[0].to_path == matched.path)
    );

    fs::remove_dir_all(root).unwrap();
}

// Tests that instruction search expands directory discovery edges.
#[test]
fn instruction_search_expands_directory_discovery_edges() {
    let root = std::env::temp_dir().join(format!(
        "termal-instruction-directory-search-{}",
        Uuid::new_v4()
    ));
    let reviewers_dir = root.join(".claude").join("reviewers");
    let commands_dir = root.join(".claude").join("commands");
    fs::create_dir_all(&reviewers_dir).unwrap();
    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("review-local.md"),
        "Discover reviewers in .claude/reviewers before running checks.\n",
    )
    .unwrap();
    fs::write(
        reviewers_dir.join("rust.md"),
        "Prefer dependency injection at unstable ownership boundaries.\n",
    )
    .unwrap();

    let response = search_instruction_phrase(&root, "dependency injection").unwrap();

    assert_eq!(response.matches.len(), 1);
    let matched = &response.matches[0];
    assert!(
        matched.path.ends_with(".claude\\reviewers\\rust.md")
            || matched.path.ends_with(".claude/reviewers/rust.md")
    );
    assert_eq!(matched.root_paths.len(), 1);
    let root_path = &matched.root_paths[0];
    assert_eq!(
        root_path.root_path,
        normalize_path_best_effort(&commands_dir.join("review-local.md"))
            .to_string_lossy()
            .into_owned()
    );
    assert_eq!(root_path.steps.len(), 1);
    assert_eq!(
        root_path.steps[0].relation,
        InstructionRelation::DirectoryDiscovery
    );
    assert_eq!(
        root_path.steps[0].to_path,
        normalize_path_best_effort(&reviewers_dir.join("rust.md"))
            .to_string_lossy()
            .into_owned()
    );

    fs::remove_dir_all(root).unwrap();
}

// Tests that instruction search stops at generic referenced docs.
#[test]
fn instruction_search_stops_at_generic_referenced_docs() {
    let root = std::env::temp_dir().join(format!(
        "termal-instruction-generic-docs-{}",
        Uuid::new_v4()
    ));
    let docs_dir = root.join("docs");
    let features_dir = docs_dir.join("features");
    fs::create_dir_all(&features_dir).unwrap();
    fs::write(
        root.join("AGENTS.md"),
        "See README.md for additional context.\n",
    )
    .unwrap();
    fs::write(
        root.join("README.md"),
        "- [docs/bugs.md](docs/bugs.md) - implementation backlog\n",
    )
    .unwrap();
    fs::write(
        docs_dir.join("bugs.md"),
        "- [Instruction Debugger](./features/instruction-debugger.md)\n",
    )
    .unwrap();
    fs::write(
        features_dir.join("instruction-debugger.md"),
        "Prefer dependency injection when debugging instruction graphs.\n",
    )
    .unwrap();

    let response = search_instruction_phrase(&root, "dependency injection").unwrap();

    assert!(response.matches.is_empty());

    fs::remove_dir_all(root).unwrap();
}

// Tests that instruction search walks instructionish docs transitively.
#[test]
fn instruction_search_walks_instructionish_docs_transitively() {
    let root =
        std::env::temp_dir().join(format!("termal-instruction-transitive-{}", Uuid::new_v4()));
    let instructions_dir = root.join("docs").join("instructions");
    fs::create_dir_all(&instructions_dir).unwrap();
    fs::write(
        root.join("AGENTS.md"),
        "Use docs/instructions/backend.md for service rules.\n",
    )
    .unwrap();
    fs::write(
        instructions_dir.join("backend.md"),
        "See shared.md for composition guidance.\n",
    )
    .unwrap();
    fs::write(
        instructions_dir.join("shared.md"),
        "Prefer dependency injection when module boundaries shift.\n",
    )
    .unwrap();

    let response = search_instruction_phrase(&root, "dependency injection").unwrap();

    assert_eq!(response.matches.len(), 1);
    let matched = &response.matches[0];
    assert!(
        matched.path.ends_with("docs\\instructions\\shared.md")
            || matched.path.ends_with("docs/instructions/shared.md")
    );
    assert_eq!(matched.root_paths.len(), 1);
    let root_path = &matched.root_paths[0];
    assert_eq!(
        root_path.root_path,
        normalize_path_best_effort(&root.join("AGENTS.md"))
            .to_string_lossy()
            .into_owned()
    );
    assert_eq!(root_path.steps.len(), 2);
    assert_eq!(
        root_path.steps[0].to_path,
        normalize_path_best_effort(&instructions_dir.join("backend.md"))
            .to_string_lossy()
            .into_owned()
    );
    assert_eq!(
        root_path.steps[1].to_path,
        normalize_path_best_effort(&instructions_dir.join("shared.md"))
            .to_string_lossy()
            .into_owned()
    );

    fs::remove_dir_all(root).unwrap();
}

// Tests that instruction search ignores internal TermAl roots for Claude reviewers.
#[test]
fn instruction_search_ignores_internal_termal_roots_for_claude_reviewers() {
    let root = std::env::temp_dir().join(format!(
        "termal-instruction-realtime-search-{}",
        Uuid::new_v4()
    ));
    let commands_dir = root.join(".claude").join("commands");
    let reviewers_dir = root.join(".claude").join("reviewers");
    let docs_features_dir = root.join("docs").join("features");
    let internal_skill_dir = root
        .join(".termal")
        .join("codex-home")
        .join("session-1")
        .join("skills")
        .join(".system")
        .join("skill-creator");
    fs::create_dir_all(&commands_dir).unwrap();
    fs::create_dir_all(&reviewers_dir).unwrap();
    fs::create_dir_all(&docs_features_dir).unwrap();
    fs::create_dir_all(&internal_skill_dir).unwrap();

    fs::write(
        commands_dir.join("review-local.md"),
        "Run `find .claude/reviewers -name \"*.md\" 2>/dev/null` via Bash to find all available reviewer lens files.\n",
    )
    .unwrap();
    fs::write(
        commands_dir.join("fix-bug.md"),
        "Read `docs/bugs.md` and find the matching bug entry.\n",
    )
    .unwrap();
    fs::write(
        reviewers_dir.join("react-typescript.md"),
        "5. **SSE / real-time handling**:\n",
    )
    .unwrap();
    fs::write(
        root.join("README.md"),
        "- [`docs/bugs.md`](docs/bugs.md) - implementation backlog\n",
    )
    .unwrap();
    fs::write(
        root.join("docs").join("bugs.md"),
        "- [Instruction Debugger](./features/instruction-debugger.md)\n",
    )
    .unwrap();
    fs::write(
        docs_features_dir.join("instruction-debugger.md"),
        "- a reviewer file was discovered from `.claude/reviewers/`\n",
    )
    .unwrap();
    fs::write(internal_skill_dir.join("SKILL.md"), "- README.md\n").unwrap();

    let response = search_instruction_phrase(&root, "real-time handling").unwrap();

    assert_eq!(response.matches.len(), 1);
    let matched = &response.matches[0];
    assert!(
        matched
            .path
            .ends_with(".claude\\reviewers\\react-typescript.md")
            || matched
                .path
                .ends_with(".claude/reviewers/react-typescript.md")
    );
    assert_eq!(matched.root_paths.len(), 1);
    let root_path = &matched.root_paths[0];
    assert_eq!(
        root_path.root_path,
        normalize_path_best_effort(&commands_dir.join("review-local.md"))
            .to_string_lossy()
            .into_owned()
    );
    assert_eq!(root_path.steps.len(), 1);
    assert_eq!(
        root_path.steps[0].relation,
        InstructionRelation::DirectoryDiscovery
    );
    assert_eq!(
        root_path.steps[0].to_path,
        normalize_path_best_effort(&reviewers_dir.join("react-typescript.md"))
            .to_string_lossy()
            .into_owned()
    );

    fs::remove_dir_all(root).unwrap();
}

// Tests that instruction search returns not found for missing session.
#[test]
fn instruction_search_returns_not_found_for_missing_session() {
    let state = test_app_state();
    let error = state
        .search_instructions("missing-session", "dependency injection")
        .unwrap_err();

    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert_eq!(error.message, "session not found");
}

// Tests that creates Claude sessions with default ask mode.
#[test]
fn creates_claude_sessions_with_default_ask_mode() {
    let mut inner = StateInner::new();

    let record = inner.create_session(Agent::Claude, None, "/tmp".to_owned(), None, None);

    assert_eq!(record.session.model, "default");
    assert_eq!(
        record.session.claude_approval_mode,
        Some(ClaudeApprovalMode::Ask)
    );
    assert_eq!(
        record.session.claude_effort,
        Some(ClaudeEffortLevel::Default)
    );
    assert_eq!(record.session.approval_policy, None);
    assert_eq!(record.session.sandbox_mode, None);
}

// Tests that Claude's default model delegates to Claude Code instead of forcing Sonnet.
#[test]
fn claude_default_model_delegates_to_claude_cli_default() {
    assert_eq!(Agent::Claude.default_model(), "default");
    assert_eq!(claude_cli_model_arg("default"), None);
    assert_eq!(claude_cli_model_arg(" Default "), None);
    assert_eq!(claude_cli_model_arg("opus"), Some("opus"));
    assert_eq!(
        claude_cli_oneshot_args(
            " default ",
            ClaudeApprovalMode::Ask,
            ClaudeEffortLevel::Default,
            ClaudeCliSessionArg::SessionId("session-a"),
        ),
        vec![
            "-p",
            "--verbose",
            "--output-format",
            "stream-json",
            "--include-partial-messages",
            "--session-id",
            "session-a",
        ],
    );
    assert_eq!(
        claude_cli_oneshot_args(
            "opus",
            ClaudeApprovalMode::Ask,
            ClaudeEffortLevel::Default,
            ClaudeCliSessionArg::SessionId("session-a"),
        ),
        vec![
            "--model",
            "opus",
            "-p",
            "--verbose",
            "--output-format",
            "stream-json",
            "--include-partial-messages",
            "--session-id",
            "session-a",
        ],
    );
    assert_eq!(
        claude_cli_persistent_args(
            "opus",
            ClaudeApprovalMode::Plan,
            ClaudeEffortLevel::High,
            Some("claude-session"),
        ),
        vec![
            "--model",
            "opus",
            "-p",
            "--verbose",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
            "--include-partial-messages",
            "--permission-prompt-tool",
            "stdio",
            "--permission-mode",
            "plan",
            "--effort",
            "high",
            "--resume",
            "claude-session",
        ],
    );
    assert_eq!(
        claude_cli_persistent_args(
            " default ",
            ClaudeApprovalMode::Ask,
            ClaudeEffortLevel::Default,
            None,
        ),
        vec![
            "-p",
            "--verbose",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
            "--include-partial-messages",
            "--permission-prompt-tool",
            "stdio",
        ],
    );
}

// Tests that creates Claude sessions with requested plan mode.
#[test]
fn creates_claude_sessions_with_requested_plan_mode() {
    let state = test_app_state();

    let response = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Plan Claude".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: Some(ClaudeApprovalMode::Plan),
            claude_effort: Some(ClaudeEffortLevel::High),
            gemini_approval_mode: None,
        })
        .unwrap();
    let session = response
        .state
        .sessions
        .iter()
        .find(|session| session.id == response.session_id)
        .expect("created session should be present");

    assert_eq!(session.claude_approval_mode, Some(ClaudeApprovalMode::Plan));
    assert_eq!(session.claude_effort, Some(ClaudeEffortLevel::High));
}

// Tests that hidden Claude spares are filtered from snapshots and persistence.
#[test]
fn hidden_claude_spares_are_filtered_from_snapshots_and_persistence() {
    let state = test_app_state();
    let workdir = resolve_session_workdir("/tmp").expect("test workdir should resolve");
    let hidden_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .ensure_hidden_claude_spare(
                workdir,
                None,
                Agent::Claude.default_model().to_owned(),
                ClaudeApprovalMode::Ask,
                ClaudeEffortLevel::Default,
            )
            .expect("hidden Claude spare should be created")
    };

    let snapshot = state.snapshot();
    assert!(
        snapshot
            .sessions
            .iter()
            .all(|session| session.id != hidden_session_id)
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .sessions
            .iter()
            .any(|record| record.hidden && record.session.id == hidden_session_id)
    );
    let persisted = PersistedState::from_inner(&inner);
    assert!(
        persisted
            .sessions
            .iter()
            .all(|record| record.session.id != hidden_session_id)
    );
}

// Tests that create session promotes matching hidden Claude spare and replenishes pool.
#[test]
fn create_session_promotes_matching_hidden_claude_spare_and_replenishes_pool() {
    let state = test_app_state();
    let workdir = resolve_session_workdir("/tmp").expect("test workdir should resolve");
    let hidden_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .ensure_hidden_claude_spare(
                workdir.clone(),
                None,
                Agent::Claude.default_model().to_owned(),
                ClaudeApprovalMode::Ask,
                ClaudeEffortLevel::Default,
            )
            .expect("hidden Claude spare should be created")
    };

    let response = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Visible Claude".to_owned()),
            workdir: Some(workdir.clone()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    assert_eq!(response.session_id, hidden_session_id);
    let session = response
        .state
        .sessions
        .iter()
        .find(|session| session.id == hidden_session_id)
        .expect("promoted hidden session should be visible");
    assert_eq!(session.name, "Visible Claude");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let promoted = inner
        .sessions
        .iter()
        .find(|record| record.session.id == hidden_session_id)
        .expect("promoted session record should exist");
    assert!(!promoted.hidden);

    let hidden_spares = inner
        .sessions
        .iter()
        .filter(|record| {
            record.hidden
                && record.session.agent == Agent::Claude
                && record.session.workdir == workdir
                && record.session.project_id.is_none()
        })
        .collect::<Vec<_>>();
    assert_eq!(hidden_spares.len(), 1);
    assert_ne!(hidden_spares[0].session.id, hidden_session_id);
}

// Tests that create session promotes matching non default hidden Claude spare.
#[test]
fn create_session_promotes_matching_non_default_hidden_claude_spare() {
    let state = test_app_state();
    let workdir = resolve_session_workdir("/tmp").expect("test workdir should resolve");
    let hidden_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .ensure_hidden_claude_spare(
                workdir.clone(),
                None,
                "claude-custom".to_owned(),
                ClaudeApprovalMode::Plan,
                ClaudeEffortLevel::High,
            )
            .expect("hidden Claude spare should be created")
    };

    let response = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Plan Claude".to_owned()),
            workdir: Some(workdir.clone()),
            project_id: None,
            model: Some("claude-custom".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: Some(ClaudeApprovalMode::Plan),
            claude_effort: Some(ClaudeEffortLevel::High),
            gemini_approval_mode: None,
        })
        .unwrap();

    assert_eq!(response.session_id, hidden_session_id);
    let inner = state.inner.lock().expect("state mutex poisoned");
    let hidden_spares = inner
        .sessions
        .iter()
        .filter(|record| {
            record.hidden
                && record.session.agent == Agent::Claude
                && record.session.workdir == workdir
                && record.session.model == "claude-custom"
                && record.session.claude_approval_mode == Some(ClaudeApprovalMode::Plan)
                && record.session.claude_effort == Some(ClaudeEffortLevel::High)
        })
        .collect::<Vec<_>>();
    assert_eq!(hidden_spares.len(), 1);
    assert_ne!(hidden_spares[0].session.id, hidden_session_id);
}

// Tests that killing last visible Claude session reaps hidden spare for context.
#[test]
fn killing_last_visible_claude_session_reaps_hidden_spare_for_context() {
    let state = test_app_state();
    let workdir = resolve_session_workdir("/tmp").expect("test workdir should resolve");
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude Visible".to_owned()),
            workdir: Some(workdir.clone()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert!(inner.sessions.iter().any(|record| {
            record.hidden
                && record.session.agent == Agent::Claude
                && record.session.workdir == workdir
        }));
    }

    let killed = state.kill_session(&created.session_id).unwrap();
    assert!(
        killed
            .sessions
            .iter()
            .all(|session| session.id != created.session_id)
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.sessions.iter().all(|record| {
        !(record.session.agent == Agent::Claude && record.session.workdir == workdir)
    }));
}

// Tests that killing one visible Claude session keeps hidden spares when another visible session remains.
#[test]
fn killing_one_visible_claude_session_keeps_hidden_spares_when_another_visible_session_remains() {
    let state = test_app_state();
    let workdir = resolve_session_workdir("/tmp").expect("test workdir should resolve");
    let first = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude A".to_owned()),
            workdir: Some(workdir.clone()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let second = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude B".to_owned()),
            workdir: Some(workdir.clone()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    state.kill_session(&first.session_id).unwrap();

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .sessions
            .iter()
            .any(|record| !record.hidden && record.session.id == second.session_id)
    );
    assert!(inner.sessions.iter().any(|record| {
        record.hidden
            && record.session.agent == Agent::Claude
            && record.session.workdir == workdir
            && record.session.project_id.is_none()
    }));
}

// Tests that killing session persists removal even when shared Codex interrupt fails.
#[test]
fn killing_session_persists_removal_even_when_shared_codex_interrupt_fails() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let shared_runtime = SharedCodexRuntime {
        runtime_id: "runtime-1".to_owned(),
        input_tx: input_tx.clone(),
        process: process.clone(),
        sessions: SharedCodexSessions::new(),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    drop(input_rx);

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: "runtime-1".to_owned(),
            input_tx,
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: shared_runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    shared_runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-1".to_owned()),
                turn_id: Some("turn-1".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    shared_runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("thread-1".to_owned(), session_id.clone());

    let killed = state.kill_session(&session_id).unwrap();
    assert!(
        killed
            .sessions
            .iter()
            .all(|session| session.id != session_id)
    );

    let reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    assert!(
        reloaded_inner
            .sessions
            .iter()
            .all(|record| record.session.id != session_id)
    );
    assert!(
        wait_for_shared_child_exit_timeout(
            &process,
            Duration::from_millis(50),
            "shared Codex runtime"
        )
        .unwrap()
        .is_none()
    );
    assert!(
        !shared_runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned")
            .contains_key(&session_id)
    );
    assert!(
        !shared_runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("thread-1")
    );

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that kill session route returns ok when shared Codex interrupt fails.
#[tokio::test]
async fn kill_session_route_returns_ok_when_shared_codex_interrupt_fails() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let shared_runtime = SharedCodexRuntime {
        runtime_id: "runtime-route".to_owned(),
        input_tx: input_tx.clone(),
        process: process.clone(),
        sessions: SharedCodexSessions::new(),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    drop(input_rx);

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: "runtime-route".to_owned(),
            input_tx,
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: shared_runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    shared_runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-route".to_owned()),
                turn_id: Some("turn-route".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    shared_runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("thread-route".to_owned(), session_id.clone());

    let app = app_router(state.clone());
    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/kill"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        response
            .sessions
            .iter()
            .all(|session| session.id != session_id)
    );
    assert!(
        wait_for_shared_child_exit_timeout(
            &process,
            Duration::from_millis(50),
            "shared Codex runtime"
        )
        .unwrap()
        .is_none()
    );

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that killing shared Codex session does not reset other shared sessions when interrupt fails.
#[test]
fn killing_shared_codex_session_does_not_reset_other_shared_sessions_when_interrupt_fails() {
    let state = test_app_state();
    let first_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Two".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let second_session_id = created.session_id;
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let shared_runtime = SharedCodexRuntime {
        runtime_id: "runtime-shared".to_owned(),
        input_tx: input_tx.clone(),
        process: process.clone(),
        sessions: SharedCodexSessions::new(),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    drop(input_rx);

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        for session_id in [&first_session_id, &second_session_id] {
            let index = inner
                .find_session_index(session_id)
                .expect("test session should exist");
            inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                runtime_id: "runtime-shared".to_owned(),
                input_tx: input_tx.clone(),
                process: process.clone(),
                shared_session: Some(SharedCodexSessionHandle {
                    runtime: shared_runtime.clone(),
                    session_id: session_id.to_string(),
                }),
            });
            inner.sessions[index].session.status = SessionStatus::Active;
        }
    }

    shared_runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .extend([
            (
                first_session_id.clone(),
                SharedCodexSessionState {
                    thread_id: Some("thread-a".to_owned()),
                    turn_id: Some("turn-a".to_owned()),
                    ..SharedCodexSessionState::default()
                },
            ),
            (
                second_session_id.clone(),
                SharedCodexSessionState {
                    thread_id: Some("thread-b".to_owned()),
                    turn_id: Some("turn-b".to_owned()),
                    ..SharedCodexSessionState::default()
                },
            ),
        ]);
    shared_runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .extend([
            ("thread-a".to_owned(), first_session_id.clone()),
            ("thread-b".to_owned(), second_session_id.clone()),
        ]);

    let killed = state.kill_session(&first_session_id).unwrap();
    assert!(
        killed
            .sessions
            .iter()
            .all(|session| session.id != first_session_id)
    );
    assert!(
        killed
            .sessions
            .iter()
            .any(|session| session.id == second_session_id)
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let second_record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == second_session_id)
        .expect("second session should still exist");
    assert!(matches!(second_record.runtime, SessionRuntime::Codex(_)));
    assert_eq!(second_record.session.status, SessionStatus::Active);
    drop(inner);

    let shared_sessions = shared_runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    assert!(!shared_sessions.contains_key(&first_session_id));
    assert!(shared_sessions.contains_key(&second_session_id));
    drop(shared_sessions);
    let thread_sessions = shared_runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned");
    assert!(!thread_sessions.contains_key("thread-a"));
    assert_eq!(
        thread_sessions.get("thread-b").map(String::as_str),
        Some(second_session_id.as_str())
    );
    drop(thread_sessions);
    assert!(
        wait_for_shared_child_exit_timeout(
            &process,
            Duration::from_millis(50),
            "shared Codex runtime"
        )
        .unwrap()
        .is_none()
    );

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that killing local Codex session prevents rediscovery after restart.
#[test]
fn killing_local_codex_session_prevents_rediscovery_after_restart() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "thread-killed".to_owned())
        .unwrap();

    let killed = state.kill_session(&session_id).unwrap();
    assert!(
        killed
            .sessions
            .iter()
            .all(|session| session.id != session_id)
    );

    let mut reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    assert!(
        reloaded_inner
            .ignored_discovered_codex_thread_ids
            .contains("thread-killed")
    );

    reloaded_inner.import_discovered_codex_threads(
        "/tmp",
        vec![DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::Never),
            archived: false,
            cwd: "/tmp".to_owned(),
            id: "thread-killed".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            title: "Killed thread".to_owned(),
        }],
    );

    assert!(
        reloaded_inner
            .sessions
            .iter()
            .all(|record| record.external_session_id.as_deref() != Some("thread-killed"))
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that setting non Codex external session ID does not clear ignored Codex thread.
#[test]
fn setting_non_codex_external_session_id_does_not_clear_ignored_codex_thread() {
    let state = test_app_state();
    let killed_codex_session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&killed_codex_session_id, "thread-shared".to_owned())
        .unwrap();
    state.kill_session(&killed_codex_session_id).unwrap();

    let cursor_session_id = test_session_id(&state, Agent::Cursor);
    state
        .set_external_session_id(&cursor_session_id, "thread-shared".to_owned())
        .unwrap();

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .ignored_discovered_codex_thread_ids
            .contains("thread-shared")
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that import discovered Codex threads prunes stale ignored thread IDs.
#[test]
fn import_discovered_codex_threads_prunes_stale_ignored_thread_ids() {
    let mut inner = StateInner::new();
    inner
        .ignored_discovered_codex_thread_ids
        .extend(["thread-live".to_owned(), "thread-stale".to_owned()]);

    inner.import_discovered_codex_threads(
        "/tmp/termal",
        vec![DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::Never),
            archived: false,
            cwd: "/tmp/termal".to_owned(),
            id: "thread-live".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            title: "Still around".to_owned(),
        }],
    );

    assert!(
        inner
            .ignored_discovered_codex_thread_ids
            .contains("thread-live")
    );
    assert!(
        !inner
            .ignored_discovered_codex_thread_ids
            .contains("thread-stale")
    );
    assert!(
        inner
            .sessions
            .iter()
            .all(|record| record.external_session_id.as_deref() != Some("thread-live"))
    );
}

// Tests that persists app settings and applies them to new sessions.
#[test]
fn persists_app_settings_and_applies_them_to_new_sessions() {
    let state = test_app_state();

    let updated = state
        .update_app_settings(UpdateAppSettingsRequest {
            default_codex_reasoning_effort: Some(CodexReasoningEffort::High),
            default_claude_approval_mode: Some(ClaudeApprovalMode::AutoApprove),
            default_claude_effort: Some(ClaudeEffortLevel::Max),
            remotes: None,
        })
        .unwrap();

    assert_eq!(
        updated.preferences.default_codex_reasoning_effort,
        CodexReasoningEffort::High
    );
    assert_eq!(
        updated.preferences.default_claude_approval_mode,
        ClaudeApprovalMode::AutoApprove
    );
    assert_eq!(
        updated.preferences.default_claude_effort,
        ClaudeEffortLevel::Max
    );
    assert_eq!(updated.preferences.remotes, default_remote_configs());

    let reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    assert_eq!(
        reloaded_inner.preferences.default_codex_reasoning_effort,
        CodexReasoningEffort::High
    );
    assert_eq!(
        reloaded_inner.preferences.default_claude_approval_mode,
        ClaudeApprovalMode::AutoApprove
    );
    assert_eq!(
        reloaded_inner.preferences.default_claude_effort,
        ClaudeEffortLevel::Max
    );
    assert_eq!(reloaded_inner.preferences.remotes, default_remote_configs());

    let reloaded_state = AppState {
        default_workdir: "/tmp".to_owned(),
        persistence_path: state.persistence_path.clone(),
        orchestrator_templates_path: state.orchestrator_templates_path.clone(),
        orchestrator_templates_lock: state.orchestrator_templates_lock.clone(),
        review_documents_lock: state.review_documents_lock.clone(),
        state_events: broadcast::channel(16).0,
        delta_events: broadcast::channel(16).0,
        file_events: broadcast::channel(16).0,
        file_events_revision: Arc::new(AtomicU64::new(0)),
        persist_tx: mpsc::channel().0,
        shared_codex_runtime: Arc::new(Mutex::new(None)),
        agent_readiness_cache: Arc::new(RwLock::new(fresh_agent_readiness_cache("/tmp"))),
        agent_readiness_refresh_lock: Arc::new(Mutex::new(())),
        remote_registry: test_remote_registry(),
        remote_sse_fallback_resynced_revision: Arc::new(Mutex::new(HashMap::new())),
        terminal_local_command_semaphore: Arc::new(tokio::sync::Semaphore::new(
            TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT,
        )),
        terminal_remote_command_semaphore: Arc::new(tokio::sync::Semaphore::new(
            TERMINAL_REMOTE_COMMAND_CONCURRENCY_LIMIT,
        )),
        stopping_orchestrator_ids: Arc::new(Mutex::new(HashSet::new())),
        stopping_orchestrator_session_ids: Arc::new(Mutex::new(HashMap::new())),
        inner: Arc::new(Mutex::new(reloaded_inner)),
    };

    let codex_created = reloaded_state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Persisted Codex".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let codex_session = codex_created
        .state
        .sessions
        .iter()
        .find(|session| session.id == codex_created.session_id)
        .expect("created Codex session should be present");
    assert_eq!(
        codex_session.reasoning_effort,
        Some(CodexReasoningEffort::High)
    );

    let claude_created = reloaded_state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Persisted Claude".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let claude_session = claude_created
        .state
        .sessions
        .iter()
        .find(|session| session.id == claude_created.session_id)
        .expect("created Claude session should be present");
    assert_eq!(
        claude_session.claude_approval_mode,
        Some(ClaudeApprovalMode::AutoApprove)
    );
    assert_eq!(claude_session.claude_effort, Some(ClaudeEffortLevel::Max));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that creates Codex sessions with requested prompt defaults.
#[test]
fn creates_codex_sessions_with_requested_prompt_defaults() {
    let state = test_app_state();

    let response = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Custom Codex".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5-mini".to_owned()),
            approval_policy: Some(CodexApprovalPolicy::OnRequest),
            reasoning_effort: Some(CodexReasoningEffort::High),
            sandbox_mode: Some(CodexSandboxMode::ReadOnly),
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let session = response
        .state
        .sessions
        .iter()
        .find(|session| session.id == response.session_id)
        .expect("created session should be present");

    assert_eq!(
        session.approval_policy,
        Some(CodexApprovalPolicy::OnRequest)
    );
    assert_eq!(session.model, "gpt-5-mini");
    assert_eq!(session.reasoning_effort, Some(CodexReasoningEffort::High));
    assert_eq!(session.sandbox_mode, Some(CodexSandboxMode::ReadOnly));
    assert_eq!(session.claude_approval_mode, None);

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .find_session_index(&response.session_id)
        .map(|index| &inner.sessions[index]);
    let record = record.expect("session record should exist");
    assert_eq!(record.codex_approval_policy, CodexApprovalPolicy::OnRequest);
    assert_eq!(record.codex_reasoning_effort, CodexReasoningEffort::High);
    assert_eq!(record.codex_sandbox_mode, CodexSandboxMode::ReadOnly);
}

// Tests that updates cursor session model settings.
#[test]
fn updates_cursor_session_model_settings() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Model".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Agent),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: Some("gpt-5.3-codex".to_owned()),
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Cursor session should be present");
    assert_eq!(session.model, "gpt-5.3-codex");
}

// Tests that updates Codex session model settings without restarting runtime.
#[test]
fn updates_codex_session_model_settings_without_restarting_runtime() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Model".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: Some("gpt-5-mini".to_owned()),
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Codex session should be present");
    assert_eq!(session.model, "gpt-5-mini");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Codex session should exist");
    assert!(!record.runtime_reset_required);
}

// Tests that updates Codex reasoning effort without restarting runtime.
#[test]
fn updates_codex_reasoning_effort_without_restarting_runtime() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Effort".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5".to_owned()),
            approval_policy: None,
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: None,
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: Some(CodexReasoningEffort::High),
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Codex session should be present");
    assert_eq!(session.reasoning_effort, Some(CodexReasoningEffort::High));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Codex session should exist");
    assert_eq!(record.codex_reasoning_effort, CodexReasoningEffort::High);
    assert!(!record.runtime_reset_required);
}

// Tests that normalizes Codex reasoning effort when switching models.
#[test]
fn normalizes_codex_reasoning_effort_when_switching_models() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Model Caps".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5".to_owned()),
            approval_policy: None,
            reasoning_effort: Some(CodexReasoningEffort::Minimal),
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    state
        .sync_session_model_options(
            &created.session_id,
            None,
            vec![
                SessionModelOption {
                    label: "GPT-5".to_owned(),
                    value: "gpt-5".to_owned(),
                    description: Some("Frontier agentic coding model.".to_owned()),
                    badges: Vec::new(),
                    supported_claude_effort_levels: Vec::new(),
                    default_reasoning_effort: Some(CodexReasoningEffort::Medium),
                    supported_reasoning_efforts: vec![
                        CodexReasoningEffort::Minimal,
                        CodexReasoningEffort::Low,
                        CodexReasoningEffort::Medium,
                        CodexReasoningEffort::High,
                    ],
                },
                SessionModelOption {
                    label: "GPT-5 Codex Mini".to_owned(),
                    value: "gpt-5-codex-mini".to_owned(),
                    description: Some(
                        "Optimized for codex. Cheaper, faster, but less capable.".to_owned(),
                    ),
                    badges: Vec::new(),
                    supported_claude_effort_levels: Vec::new(),
                    default_reasoning_effort: Some(CodexReasoningEffort::Medium),
                    supported_reasoning_efforts: vec![
                        CodexReasoningEffort::Medium,
                        CodexReasoningEffort::High,
                    ],
                },
            ],
        )
        .unwrap();

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: Some("gpt-5-codex-mini".to_owned()),
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Codex session should be present");
    assert_eq!(session.model, "gpt-5-codex-mini");
    assert_eq!(session.reasoning_effort, Some(CodexReasoningEffort::Medium));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Codex session should exist");
    assert_eq!(record.codex_reasoning_effort, CodexReasoningEffort::Medium);
}

// Tests that rejects unsupported Codex reasoning effort for selected model.
#[test]
fn rejects_unsupported_codex_reasoning_effort_for_selected_model() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Invalid Effort".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5-codex-mini".to_owned()),
            approval_policy: None,
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    state
        .sync_session_model_options(
            &created.session_id,
            None,
            vec![SessionModelOption {
                label: "GPT-5 Codex Mini".to_owned(),
                value: "gpt-5-codex-mini".to_owned(),
                description: Some(
                    "Optimized for codex. Cheaper, faster, but less capable.".to_owned(),
                ),
                badges: Vec::new(),
                supported_claude_effort_levels: Vec::new(),
                default_reasoning_effort: Some(CodexReasoningEffort::Medium),
                supported_reasoning_efforts: vec![
                    CodexReasoningEffort::Medium,
                    CodexReasoningEffort::High,
                ],
            }],
        )
        .unwrap();

    let error = match state.update_session_settings(
        &created.session_id,
        UpdateSessionSettingsRequest {
            name: None,
            model: None,
            sandbox_mode: None,
            approval_policy: None,
            reasoning_effort: Some(CodexReasoningEffort::Low),
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        },
    ) {
        Ok(_) => panic!("unsupported Codex effort should be rejected"),
        Err(error) => error,
    };

    assert!(
        error
            .message
            .contains("does not support `low` reasoning effort; choose medium or high")
    );
}

// Tests that updates Claude session model settings without restarting runtime.
#[test]
fn updates_claude_session_model_settings_without_restarting_runtime() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude Model".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("sonnet".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: Some(ClaudeApprovalMode::Ask),
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let child = test_exit_success_child();
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-model-update".to_owned(),
        input_tx,
        process: Arc::new(SharedChild::new(child).unwrap()),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: Some("opus".to_owned()),
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Claude session should be present");
    assert_eq!(session.model, "opus");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Claude session should exist");
    assert!(!record.runtime_reset_required);

    let command = input_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("Claude model update should arrive");
    match command {
        ClaudeRuntimeCommand::SetModel(model) => assert_eq!(model, "opus"),
        _ => panic!("expected Claude model update command"),
    }
}

#[test]
fn updating_running_claude_session_to_default_model_requires_restart() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude Default".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("sonnet".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: Some(ClaudeApprovalMode::Ask),
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let child = test_exit_success_child();
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-default-model-update".to_owned(),
        input_tx,
        process: Arc::new(SharedChild::new(child).unwrap()),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: Some("default".to_owned()),
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Claude session should be present");
    assert_eq!(session.model, "default");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Claude session should exist");
    assert!(record.runtime_reset_required);
    drop(inner);

    match input_rx.recv_timeout(Duration::from_millis(100)) {
        Err(mpsc::RecvTimeoutError::Timeout) => {}
        Ok(_) => panic!("default sentinel should not be sent to Claude"),
        Err(err) => panic!("unexpected Claude command channel error: {err}"),
    }
}

// Tests that updates Claude effort and marks runtime for restart.
#[test]
fn updates_claude_effort_and_marks_runtime_for_restart() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude Effort".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("sonnet".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: Some(ClaudeApprovalMode::Ask),
            claude_effort: Some(ClaudeEffortLevel::Default),
            gemini_approval_mode: None,
        })
        .unwrap();

    let child = test_exit_success_child();
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-effort-update".to_owned(),
        input_tx,
        process: Arc::new(SharedChild::new(child).unwrap()),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: None,
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: Some(ClaudeEffortLevel::High),
                gemini_approval_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Claude session should be present");
    assert_eq!(session.claude_effort, Some(ClaudeEffortLevel::High));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Claude session should exist");
    assert!(record.runtime_reset_required);

    match input_rx.recv_timeout(Duration::from_millis(100)) {
        Err(mpsc::RecvTimeoutError::Timeout) => {}
        Ok(_) => panic!("Claude effort changes should not send a live runtime command"),
        Err(err) => panic!("unexpected channel error: {err}"),
    }
}

// Tests that syncs Claude model options into session state.
#[test]
fn syncs_claude_model_options_into_session_state() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude Refresh".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let model_options = vec![
        SessionModelOption::plain("Default (recommended)", "default"),
        SessionModelOption::plain("Sonnet", "sonnet"),
    ];

    state
        .sync_session_model_options(&created.session_id, None, model_options.clone())
        .expect("Claude model options should sync");

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("synced Claude session should be present");

    assert_eq!(session.model, "default");
    assert_eq!(session.model_options, model_options);
}

// Tests that refreshes Codex model options from runtime.
#[test]
fn refreshes_codex_model_options_from_runtime() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Refresh".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5.4".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let child = test_exit_success_child();
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = CodexRuntimeHandle {
        runtime_id: "codex-model-refresh".to_owned(),
        input_tx,
        process: Arc::new(SharedChild::new(child).unwrap()),
        shared_session: None,
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }

    std::thread::spawn(move || {
        let command = input_rx
            .recv()
            .expect("Codex refresh command should arrive");
        match command {
            CodexRuntimeCommand::RefreshModelList { response_tx } => {
                let _ = response_tx.send(Ok(vec![
                    SessionModelOption::plain("gpt-5.4", "gpt-5.4"),
                    SessionModelOption::plain("gpt-5.3-codex", "gpt-5.3-codex"),
                ]));
            }
            _ => panic!("expected Codex model refresh command"),
        }
    });

    let refreshed = state
        .refresh_session_model_options(&created.session_id)
        .expect("Codex model refresh should succeed");
    let session = refreshed
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("refreshed Codex session should be present");

    assert_eq!(
        session.model_options,
        vec![
            SessionModelOption::plain("gpt-5.4", "gpt-5.4"),
            SessionModelOption::plain("gpt-5.3-codex", "gpt-5.3-codex"),
        ]
    );
}

// Tests that shared Codex model-list pagination fails after the configured page cap.
#[test]
fn shared_codex_model_list_pagination_stops_after_max_pages() {
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let (input_tx, input_rx) = mpsc::channel();
    let (response_tx, response_rx) = mpsc::channel();
    let mut writer = Vec::new();

    fire_codex_model_list_page(
        &mut writer,
        &pending_requests,
        &input_tx,
        Some("cursor-50".to_owned()),
        Vec::new(),
        SHARED_CODEX_MODEL_LIST_MAX_PAGES,
        response_tx,
    )
    .unwrap();

    let (_request_id, sender) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));
    sender
        .send(Ok(json!({
            "data": [],
            "nextCursor": "cursor-51"
        })))
        .unwrap();

    let result = response_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("model list response should arrive");
    assert_eq!(
        result,
        Err(format!(
            "Codex model list pagination exceeded {} pages.",
            SHARED_CODEX_MODEL_LIST_MAX_PAGES
        ))
    );
    match input_rx.recv_timeout(Duration::from_millis(100)) {
        Err(mpsc::RecvTimeoutError::Timeout) => {}
        Ok(_) => panic!("model list pagination should not queue another page past the cap"),
        Err(err) => panic!("unexpected model list pagination channel error: {err}"),
    }
}

// Tests that shared Codex model-list pagination reports a continuation queue failure immediately.
#[test]
fn shared_codex_model_list_pagination_queue_failure_returns_error() {
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let (input_tx, input_rx) = mpsc::channel::<CodexRuntimeCommand>();
    drop(input_rx);
    let (response_tx, response_rx) = mpsc::channel();
    let mut writer = Vec::new();

    fire_codex_model_list_page(
        &mut writer,
        &pending_requests,
        &input_tx,
        Some("cursor-1".to_owned()),
        Vec::new(),
        1,
        response_tx,
    )
    .unwrap();

    let (_request_id, sender) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));
    sender
        .send(Ok(json!({
            "data": [],
            "nextCursor": "cursor-2"
        })))
        .unwrap();

    let result = response_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("model list response should arrive");
    assert_eq!(
        result,
        Err("failed to queue next Codex model list page: sending on a closed channel".to_owned())
    );
}

// Tests that fork Codex thread creates a new local session.
#[test]
fn fork_codex_thread_creates_a_new_local_session() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Review".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5.4".to_owned()),
            approval_policy: Some(CodexApprovalPolicy::Never),
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    state
        .set_external_session_id(&created.session_id, "thread-origin".to_owned())
        .unwrap();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("source Codex session should exist");
        inner.sessions[index].session.model_options =
            vec![SessionModelOption::plain("gpt-5.4", "gpt-5.4")];
    }

    let (runtime, input_rx, _process) = test_shared_codex_runtime("shared-codex-fork");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Codex fork command should arrive");
        match command {
            CodexRuntimeCommand::JsonRpcRequest {
                method,
                params,
                response_tx,
                ..
            } => {
                assert_eq!(method, "thread/fork");
                assert_eq!(params["threadId"], "thread-origin");
                let _ = response_tx.send(Ok(json!({
                    "thread": {
                        "id": "thread-forked",
                        "name": "Forked Review",
                        "preview": "Forked preview",
                        "turns": [
                            {
                                "id": "turn-1",
                                "status": "completed",
                                "items": [
                                    {
                                        "id": "item-user-1",
                                        "type": "userMessage",
                                        "content": [
                                            {
                                                "type": "text",
                                                "text": "Review src/state.rs"
                                            },
                                            {
                                                "type": "mention",
                                                "name": "docs/bugs.md",
                                                "path": "docs/bugs.md"
                                            }
                                        ]
                                    },
                                    {
                                        "id": "item-reasoning-1",
                                        "type": "reasoning",
                                        "summary": ["Inspect session state."],
                                        "content": ["Watch archive transitions."]
                                    },
                                    {
                                        "id": "item-agent-1",
                                        "type": "agentMessage",
                                        "text": "I found the bug."
                                    },
                                    {
                                        "id": "item-command-1",
                                        "type": "commandExecution",
                                        "command": "git diff --stat",
                                        "commandActions": [],
                                        "cwd": "/tmp/forked",
                                        "status": "completed",
                                        "aggregatedOutput": "1 file changed",
                                        "exitCode": 0
                                    },
                                    {
                                        "id": "item-file-1",
                                        "type": "fileChange",
                                        "status": "completed",
                                        "changes": [
                                            {
                                                "path": "src/state.rs",
                                                "diff": "@@ -1 +1 @@\n-old\n+new",
                                                "kind": {
                                                    "type": "modify"
                                                }
                                            }
                                        ]
                                    }
                                ]
                            }
                        ]
                    },
                    "model": "gpt-5.5",
                    "approvalPolicy": "on-request",
                    "sandbox": {
                        "type": "workspaceWrite"
                    },
                    "reasoningEffort": "high",
                    "cwd": "/tmp/forked",
                })));
            }
            _ => panic!("expected shared Codex JSON-RPC request"),
        }
    });

    let forked = state.fork_codex_thread(&created.session_id).unwrap();
    assert_ne!(forked.session_id, created.session_id);

    let forked_session = forked
        .state
        .sessions
        .iter()
        .find(|session| session.id == forked.session_id)
        .expect("forked session should be present");
    assert_eq!(forked_session.name, "Forked Review Fork");
    assert_eq!(forked_session.model, "gpt-5.5");
    assert_eq!(
        forked_session.approval_policy,
        Some(CodexApprovalPolicy::OnRequest)
    );
    assert_eq!(
        forked_session.reasoning_effort,
        Some(CodexReasoningEffort::High)
    );
    assert_eq!(
        forked_session.sandbox_mode,
        Some(CodexSandboxMode::WorkspaceWrite)
    );
    assert_eq!(
        forked_session.external_session_id.as_deref(),
        Some("thread-forked")
    );
    assert_eq!(
        forked_session.codex_thread_state,
        Some(CodexThreadState::Active)
    );
    assert_eq!(forked_session.workdir, "/tmp/forked");
    assert_eq!(
        forked_session.model_options,
        vec![SessionModelOption::plain("gpt-5.4", "gpt-5.4")]
    );
    assert!(matches!(
        forked_session.messages.first(),
        Some(Message::Text { author: Author::You, text, .. })
            if text.contains("Review src/state.rs")
                && text.contains("Mention: docs/bugs.md (docs/bugs.md)")
    ));
    assert!(matches!(
        forked_session.messages.get(1),
        Some(Message::Thinking { title, lines, .. })
            if title == "Codex reasoning"
                && lines == &vec![
                    "Inspect session state.".to_owned(),
                    "Watch archive transitions.".to_owned(),
                ]
    ));
    assert!(matches!(
        forked_session.messages.get(2),
        Some(Message::Text { author: Author::Assistant, text, .. }) if text == "I found the bug."
    ));
    assert!(matches!(
        forked_session.messages.get(3),
        Some(Message::Command {
            command,
            output,
            status,
            ..
        }) if command == "git diff --stat"
            && output == "1 file changed"
            && *status == CommandStatus::Success
    ));
    assert!(matches!(
        forked_session.messages.get(4),
        Some(Message::Diff {
            file_path,
            summary,
            diff,
            change_type,
            ..
        }) if file_path == "src/state.rs"
            && summary == "Updated state.rs"
            && diff.contains("+new")
            && *change_type == ChangeType::Edit
    ));
    assert!(!forked_session.messages.iter().any(
        |message| matches!(message, Message::Markdown { title, .. } if title == "Forked Codex thread")
    ));
}

// Tests that fork Codex thread falls back to note when history is unavailable.
#[test]
fn fork_codex_thread_falls_back_to_note_when_history_is_unavailable() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Review".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5.4".to_owned()),
            approval_policy: Some(CodexApprovalPolicy::Never),
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    state
        .set_external_session_id(&created.session_id, "thread-origin".to_owned())
        .unwrap();

    let (runtime, input_rx, _process) = test_shared_codex_runtime("shared-codex-fork-fallback");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Codex fork command should arrive");
        match command {
            CodexRuntimeCommand::JsonRpcRequest {
                method,
                params,
                response_tx,
                ..
            } => {
                assert_eq!(method, "thread/fork");
                assert_eq!(params["threadId"], "thread-origin");
                let _ = response_tx.send(Ok(json!({
                    "thread": {
                        "id": "thread-forked",
                        "name": "Forked Review",
                        "preview": "Forked preview"
                    },
                    "model": "gpt-5.5",
                    "approvalPolicy": "on-request",
                    "sandbox": {
                        "type": "workspaceWrite"
                    },
                    "reasoningEffort": "high",
                    "cwd": "/tmp/forked"
                })));
            }
            _ => panic!("expected shared Codex JSON-RPC request"),
        }
    });

    let forked = state.fork_codex_thread(&created.session_id).unwrap();
    let forked_session = forked
        .state
        .sessions
        .iter()
        .find(|session| session.id == forked.session_id)
        .expect("forked session should be present");
    assert!(matches!(
        forked_session.messages.last(),
        Some(Message::Markdown { title, markdown, .. })
            if title == "Forked Codex thread"
                && markdown.contains("Codex did not return the earlier thread history")
    ));
}

// Tests that Codex thread actions require a live idle thread.
#[test]
fn codex_thread_actions_require_a_live_idle_thread() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);

    let missing_thread_error = match state.archive_codex_thread(&session_id) {
        Ok(_) => panic!("archive should fail without a live Codex thread"),
        Err(err) => err,
    };
    assert!(
        missing_thread_error
            .message
            .contains("only available after the session has started a thread")
    );

    state
        .set_external_session_id(&session_id, "thread-live".to_owned())
        .unwrap();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    let busy_error = match state.compact_codex_thread(&session_id) {
        Ok(_) => panic!("compact should fail while the session is active"),
        Err(err) => err,
    };
    assert!(
        busy_error
            .message
            .contains("wait for the current Codex turn to finish")
    );

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].session.status = SessionStatus::Idle;
        let queued_message_id = inner.next_message_id();
        queue_prompt_on_record(
            &mut inner.sessions[index],
            PendingPrompt {
                attachments: Vec::new(),
                id: queued_message_id,
                timestamp: stamp_now(),
                text: "queued prompt".to_owned(),
                expanded_text: None,
            },
            Vec::new(),
        );
    }

    let queued_error = match state.archive_codex_thread(&session_id) {
        Ok(_) => panic!("archive should fail while prompts are queued"),
        Err(err) => err,
    };
    assert!(
        queued_error
            .message
            .contains("wait for queued Codex prompts to finish")
    );
}

// Tests that Codex archive and unarchive actions update thread state and block dispatch.
#[test]
fn codex_archive_and_unarchive_actions_update_thread_state_and_block_dispatch() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "thread-live".to_owned())
        .unwrap();

    let initial_session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == session_id)
        .expect("Codex session should exist");
    assert_eq!(
        initial_session.codex_thread_state,
        Some(CodexThreadState::Active)
    );

    let (runtime, input_rx, _process) = test_shared_codex_runtime("shared-codex-archive");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    std::thread::spawn(move || {
        for expected_method in ["thread/archive", "thread/unarchive"] {
            let command = input_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("Codex thread action should arrive");
            match command {
                CodexRuntimeCommand::JsonRpcRequest {
                    method,
                    params,
                    response_tx,
                    ..
                } => {
                    assert_eq!(method, expected_method);
                    assert_eq!(params["threadId"], "thread-live");
                    let _ = response_tx.send(Ok(json!({})));
                }
                _ => panic!("expected shared Codex JSON-RPC request"),
            }
        }
    });

    let archived = state.archive_codex_thread(&session_id).unwrap();
    let archived_session = archived
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated Codex session should be present");
    assert_eq!(
        archived_session.codex_thread_state,
        Some(CodexThreadState::Archived)
    );
    assert!(matches!(
        archived_session.messages.last(),
        Some(Message::Markdown { title, .. }) if title == "Archived Codex thread"
    ));

    let archived_error = match state.dispatch_turn(
        &session_id,
        SendMessageRequest {
            text: "resume the review".to_owned(),
            expanded_text: None,
            attachments: Vec::new(),
        },
    ) {
        Ok(_) => panic!("archived Codex thread should reject new prompts"),
        Err(err) => err,
    };
    assert_eq!(archived_error.status, StatusCode::CONFLICT);
    assert!(
        archived_error
            .message
            .contains("current Codex thread is archived")
    );

    let restored = state.unarchive_codex_thread(&session_id).unwrap();
    let restored_session = restored
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated Codex session should be present");
    assert_eq!(
        restored_session.codex_thread_state,
        Some(CodexThreadState::Active)
    );
    assert!(matches!(
        restored_session.messages.last(),
        Some(Message::Markdown { title, .. }) if title == "Restored Codex thread"
    ));
}

// Tests that shared Codex archive notifications update thread state.
#[test]
fn shared_codex_archive_notifications_update_thread_state() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "conversation-123".to_owned())
        .unwrap();
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-thread-state");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let archived = json!({
        "method": "thread/archived",
        "params": {
            "threadId": "conversation-123"
        }
    });
    let unarchived = json!({
        "method": "thread/unarchived",
        "params": {
            "threadId": "conversation-123"
        }
    });

    handle_shared_codex_app_server_message(
        &archived,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let archived_session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(
        archived_session.codex_thread_state,
        Some(CodexThreadState::Archived)
    );

    handle_shared_codex_app_server_message(
        &unarchived,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let restored_session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(
        restored_session.codex_thread_state,
        Some(CodexThreadState::Active)
    );
}

// Tests that shared Codex model rerouted notification records notice.
#[test]
fn shared_codex_model_rerouted_notification_records_notice() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "conversation-reroute".to_owned())
        .unwrap();
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-reroute");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-reroute".to_owned()),
                turn_id: Some("turn-reroute".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-reroute".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let rerouted = json!({
        "method": "model/rerouted",
        "params": {
            "threadId": "conversation-reroute",
            "turnId": "turn-reroute",
            "fromModel": "gpt-5.4",
            "toModel": "gpt-5.4-mini",
            "reason": "highRiskCyberActivity"
        }
    });

    handle_shared_codex_app_server_message(
        &rerouted,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. })
            if text == "Codex rerouted this turn from `gpt-5.4` to `gpt-5.4-mini` because it detected high-risk cyber activity."
    ));
}

// Tests that shared Codex compaction notice inserts before visible assistant output.
#[test]
fn shared_codex_compaction_notice_inserts_before_visible_assistant_output() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "conversation-compact".to_owned())
        .unwrap();
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-compact");

    let assistant_message_id = state.allocate_message_id();
    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: assistant_message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Existing assistant output".to_owned(),
                expanded_text: None,
            },
        )
        .unwrap();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-compact".to_owned()),
                turn_id: Some("turn-compact".to_owned()),
                turn_state: CodexTurnState {
                    assistant_output_started: true,
                    first_visible_assistant_message_id: Some(assistant_message_id.clone()),
                    ..CodexTurnState::default()
                },
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-compact".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let compacted = json!({
        "method": "thread/compacted",
        "params": {
            "threadId": "conversation-compact",
            "turnId": "turn-compact"
        }
    });

    handle_shared_codex_app_server_message(
        &compacted,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    let compact_notice_index = session
        .messages
        .iter()
        .position(|message| {
            matches!(
                message,
                Message::Text { text, .. }
                    if text == "Codex compacted the thread context for this turn."
            )
        })
        .expect("compaction notice should be present");
    let assistant_index = session
        .messages
        .iter()
        .position(|message| {
            matches!(
                message,
                Message::Text { id, text, .. }
                    if id == &assistant_message_id && text == "Existing assistant output"
            )
        })
        .expect("assistant output should remain present");
    assert!(compact_notice_index < assistant_index);
}

// Tests that ACP initialize reads load-session support from agent capabilities.
#[test]
fn acp_supports_session_load_reads_agent_capabilities() {
    assert_eq!(
        acp_supports_session_load(&json!({
            "agentCapabilities": {
                "loadSession": false,
            }
        })),
        Some(false)
    );
    assert_eq!(
        acp_supports_session_load(&json!({
            "agentCapabilities": {
                "loadSession": true,
            }
        })),
        Some(true)
    );
}

// Tests that ACP initialize also reads legacy capability envelopes.
#[test]
fn acp_supports_session_load_reads_legacy_capabilities() {
    assert_eq!(
        acp_supports_session_load(&json!({
            "capabilities": {
                "loadSession": false,
            }
        })),
        Some(false)
    );
    assert_eq!(acp_supports_session_load(&json!({})), None);
}

// Tests that ACP runtimes do not assume session/load support before initialize reports it.
#[test]
fn acp_runtime_state_defaults_session_load_support_to_unknown() {
    assert_eq!(AcpRuntimeState::default().supports_session_load, None);
}

// Tests that ACP resumes still attempt session/load when initialize omitted the capability bit.
#[test]
fn acp_session_resume_attempts_load_when_session_load_support_is_unknown() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Resume".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Ask),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("Cursor session should be created");
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState::default()));
    let writer = SharedBufferWriter::default();
    let thread_writer = writer.clone();
    let thread_pending_requests = pending_requests.clone();
    let thread_state = state.clone();
    let thread_runtime_state = runtime_state.clone();
    let thread_session_id = created.session_id.clone();
    let handle = std::thread::spawn(move || {
        let mut stdin = thread_writer;
        ensure_acp_session_ready(
            &mut stdin,
            &thread_pending_requests,
            &thread_state,
            &thread_session_id,
            &thread_runtime_state,
            AcpAgent::Cursor,
            &AcpPromptCommand {
                cwd: "/tmp".to_owned(),
                cursor_mode: Some(CursorMode::Ask),
                model: "auto".to_owned(),
                prompt: "Resume the prior session".to_owned(),
                resume_session_id: Some("cursor-session-1".to_owned()),
            },
        )
    });

    let (_load_request_id, load_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    load_sender
        .send(Ok(json!({
            "configOptions": [
                {
                    "id": "model",
                    "currentValue": "auto",
                    "options": [
                        {
                            "value": "auto",
                            "name": "Auto"
                        }
                    ]
                },
                {
                    "id": "mode",
                    "currentValue": "ask",
                    "options": [
                        {
                            "value": "ask",
                            "name": "Ask"
                        }
                    ]
                }
            ]
        })))
        .expect("session/load response should send");

    let external_session_id = handle
        .join()
        .expect("Cursor ACP worker should finish")
        .expect("Cursor resume should reuse the persisted session");
    assert_eq!(external_session_id, "cursor-session-1");

    let written = writer.contents();
    assert!(
        written.contains("\"method\":\"session/load\""),
        "session/load request should be written\n{written}"
    );
    assert!(
        !written.contains("\"method\":\"session/new\""),
        "session/new should not be written when resuming with unknown capability support\n{written}"
    );

    let session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Cursor session should be present");
    assert_eq!(
        session.external_session_id.as_deref(),
        Some("cursor-session-1")
    );

    let runtime_state = runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned");
    assert_eq!(
        runtime_state.current_session_id.as_deref(),
        Some("cursor-session-1")
    );
    assert_eq!(runtime_state.supports_session_load, Some(true));
}

// Tests that ACP skips session/load when initialize explicitly reports it unsupported.
#[test]
fn acp_session_resume_skips_load_when_session_load_is_explicitly_unsupported() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Resume".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Ask),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("Cursor session should be created");
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: None,
        is_loading_history: false,
        supports_session_load: Some(false),
    }));
    let writer = SharedBufferWriter::default();
    let thread_writer = writer.clone();
    let thread_pending_requests = pending_requests.clone();
    let thread_state = state.clone();
    let thread_runtime_state = runtime_state.clone();
    let thread_session_id = created.session_id.clone();
    let handle = std::thread::spawn(move || {
        let mut stdin = thread_writer;
        ensure_acp_session_ready(
            &mut stdin,
            &thread_pending_requests,
            &thread_state,
            &thread_session_id,
            &thread_runtime_state,
            AcpAgent::Cursor,
            &AcpPromptCommand {
                cwd: "/tmp".to_owned(),
                cursor_mode: Some(CursorMode::Ask),
                model: "auto".to_owned(),
                prompt: "Resume the prior session".to_owned(),
                resume_session_id: Some("cursor-session-1".to_owned()),
            },
        )
    });

    let (_new_request_id, new_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    new_sender
        .send(Ok(json!({
            "sessionId": "cursor-session-new",
            "configOptions": [
                {
                    "id": "model",
                    "currentValue": "auto",
                    "options": [
                        {
                            "value": "auto",
                            "name": "Auto"
                        }
                    ]
                },
                {
                    "id": "mode",
                    "currentValue": "ask",
                    "options": [
                        {
                            "value": "ask",
                            "name": "Ask"
                        }
                    ]
                }
            ]
        })))
        .expect("session/new response should send");

    let external_session_id = handle
        .join()
        .expect("Cursor ACP worker should finish")
        .expect("Cursor resume should start a fresh ACP session");
    assert_eq!(external_session_id, "cursor-session-new");

    let written = writer.contents();
    assert!(
        !written.contains("\"method\":\"session/load\""),
        "session/load should not be written when support is explicitly unavailable\n{written}"
    );
    assert!(
        written.contains("\"method\":\"session/new\""),
        "session/new should be written when support is explicitly unavailable\n{written}"
    );

    let session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Cursor session should be present");
    assert_eq!(
        session.external_session_id.as_deref(),
        Some("cursor-session-new")
    );

    let runtime_state = runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned");
    assert_eq!(
        runtime_state.current_session_id.as_deref(),
        Some("cursor-session-new")
    );
    assert_eq!(runtime_state.supports_session_load, Some(false));
}
// Tests that Gemini invalid-session detection searches wrapped anyhow error chains.
#[test]
fn gemini_invalid_session_load_error_matches_wrapped_chain_messages() {
    let err = anyhow::anyhow!("Invalid session identifier").context("session/load failed");
    assert!(is_gemini_invalid_session_load_error(&err));
}

// Tests that ACP invalid-session data inspection handles wrapper fields and depth limits.
#[test]
fn acp_invalid_session_identifier_detection_handles_wrappers_and_depth_limits() {
    assert!(acp_error_data_indicates_invalid_session_identifier(
        &json!({
            "details": [{
                "error": "invalidSessionId"
            }]
        })
    ));

    let mut boundary = json!("invalidSessionIdentifier");
    for _ in 0..10 {
        boundary = json!({ "details": boundary });
    }
    assert!(acp_error_data_indicates_invalid_session_identifier(
        &boundary
    ));

    let mut nested = json!("invalidSessionIdentifier");
    for _ in 0..11 {
        nested = json!({ "details": nested });
    }
    assert!(!acp_error_data_indicates_invalid_session_identifier(
        &nested
    ));
}

// Tests that Gemini settings overrides preserve existing fields while disabling interactive shell.
#[test]
fn disable_gemini_interactive_shell_in_settings_preserves_other_values() {
    let mut settings = json!({
        "security": {
            "auth": {
                "selectedType": "oauth-personal"
            }
        },
        "tools": {
            "shell": {
                "enableInteractiveShell": true,
                "pager": "less"
            }
        }
    });

    disable_gemini_interactive_shell_in_settings(&mut settings);

    assert_eq!(
        settings.pointer("/tools/shell/enableInteractiveShell"),
        Some(&Value::Bool(false))
    );
    assert_eq!(
        settings.pointer("/tools/shell/pager"),
        Some(&Value::String("less".to_owned()))
    );
    assert_eq!(
        settings.pointer("/security/auth/selectedType"),
        Some(&Value::String("oauth-personal".to_owned()))
    );
}

// Tests that Gemini settings overrides create the full shell path from an empty object.
#[test]
fn disable_gemini_interactive_shell_in_settings_builds_shell_path_from_empty_object() {
    let mut settings = json!({});

    disable_gemini_interactive_shell_in_settings(&mut settings);

    assert_eq!(
        settings.pointer("/tools/shell/enableInteractiveShell"),
        Some(&Value::Bool(false))
    );
}

// Tests that malformed Gemini settings do not block the Windows override path.
#[test]
fn load_gemini_settings_json_ignores_malformed_input() {
    let settings_path =
        std::env::temp_dir().join(format!("termal-gemini-settings-invalid-{}", Uuid::new_v4()));
    fs::write(
        &settings_path,
        r#"{"security": { "auth": { "selectedType": "oauth-personal" }"#,
    )
    .expect("invalid Gemini settings should be written");

    let loaded = load_gemini_settings_json(Some(settings_path.as_path()));
    assert_eq!(loaded, json!({}));
    assert_eq!(
        gemini_selected_auth_type_from_settings_file(settings_path.as_path()),
        None
    );

    let _ = fs::remove_file(settings_path);
}

// Tests that Gemini ACP launch ignores repository dotenv files for child env injection.
#[test]
fn gemini_dotenv_env_pairs_ignore_workspace_env_files() {
    let project_root =
        std::env::temp_dir().join(format!("termal-gemini-dotenv-env-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should be created");
    fs::write(
        project_root.join(".env"),
        "GEMINI_API_KEY=dotenv-gemini-key\nexport GOOGLE_API_KEY='vertex-key'\nGOOGLE_CLOUD_PROJECT=demo-project\nGOOGLE_CLOUD_LOCATION=us-central1\n",
    )
    .expect("Gemini dotenv file should be written");

    let overrides = gemini_dotenv_env_pairs()
        .into_iter()
        .collect::<HashMap<_, _>>();

    assert!(overrides.is_empty());

    let _ = fs::remove_dir_all(project_root);
}

// Tests that Gemini dotenv lookup resolves home-directory files without walking the workdir.
#[test]
fn find_gemini_env_file_reads_home_directory_env_files() {
    let _env_lock = TEST_HOME_ENV_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home_dir = std::env::temp_dir().join(format!("termal-gemini-home-env-{}", Uuid::new_v4()));
    let gemini_dir = home_dir.join(".gemini");
    fs::create_dir_all(&gemini_dir).expect("Gemini home directory should be created");

    {
        let _home_env = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home_dir);
        assert_eq!(find_gemini_env_file(), None);
        let gemini_env = gemini_dir.join(".env");
        fs::write(&gemini_env, "GEMINI_API_KEY=home-gemini-key\n")
            .expect("Gemini home env should be written");
        assert_eq!(find_gemini_env_file(), Some(gemini_env.clone()));

        fs::remove_file(&gemini_env).expect("Gemini home env should be removed");
        let fallback_env = home_dir.join(".env");
        fs::write(&fallback_env, "GEMINI_API_KEY=home-fallback-key\n")
            .expect("home fallback env should be written");
        assert_eq!(find_gemini_env_file(), Some(fallback_env));
    }

    let _ = fs::remove_dir_all(home_dir);
}

// Tests that Gemini ACP auth selection ignores workspace dotenv credentials.
#[test]
fn select_acp_auth_method_ignores_workspace_dotenv_credentials() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-gemini-auth-method-dotenv-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should be created");
    fs::write(
        project_root.join(".env"),
        "GEMINI_API_KEY=dotenv-gemini-key\n",
    )
    .expect("Gemini dotenv file should be written");

    let initialize_result = json!({
        "authMethods": [
            { "id": "vertex-ai" },
            { "id": "gemini-api-key" }
        ]
    });
    assert_eq!(
        select_acp_auth_method(
            &initialize_result,
            AcpAgent::Gemini,
            project_root
                .to_str()
                .expect("temp path should be valid UTF-8"),
        ),
        None
    );

    let _ = fs::remove_dir_all(project_root);
}

// Tests that TermAl prepares a Windows Gemini system-settings override file.
#[test]
fn prepare_termal_gemini_system_settings_writes_override_file() {
    if !cfg!(windows) {
        return;
    }

    let project_root =
        std::env::temp_dir().join(format!("termal-gemini-system-settings-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("Gemini override project root should be created");
    let workdir = project_root
        .to_str()
        .expect("test workdir should be valid UTF-8");

    let settings_path = prepare_termal_gemini_system_settings(workdir)
        .expect("Gemini settings override should prepare")
        .expect("Windows should create a Gemini settings override");
    let written: Value = serde_json::from_str(
        &fs::read_to_string(&settings_path).expect("Gemini override file should be readable"),
    )
    .expect("Gemini override file should parse");

    assert_eq!(
        written.pointer("/tools/shell/enableInteractiveShell"),
        Some(&Value::Bool(false))
    );

    let _ = fs::remove_dir_all(project_root);
}

// Tests that Gemini interactive-shell warnings explain the TermAl override on Windows.
#[test]
fn gemini_interactive_shell_warning_respects_workspace_settings() {
    if !cfg!(windows) {
        return;
    }

    // Hold the home-env mutex so this test's USERPROFILE and
    // GEMINI_CLI_SYSTEM_SETTINGS_PATH redirects don't race with other
    // home-env tests that run in parallel.
    let _env_lock = TEST_HOME_ENV_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let project_root = std::env::temp_dir().join(format!(
        "termal-gemini-interactive-shell-{}",
        Uuid::new_v4()
    ));
    let settings_dir = project_root.join(".gemini");
    fs::create_dir_all(&settings_dir).expect("Gemini settings directory should be created");
    let settings_path = settings_dir.join("settings.json");
    let workdir = project_root
        .to_str()
        .expect("test workdir should be valid UTF-8");

    // Point GEMINI_CLI_SYSTEM_SETTINGS_PATH at a path that does not exist so
    // the real C:\ProgramData\gemini-cli\settings.json (written by TermAl with
    // enableInteractiveShell=false) does not shadow the project setting we are
    // testing here.
    let absent_system_settings = project_root.join("no-system-settings.json");
    let _system_env =
        ScopedEnvVar::set_path("GEMINI_CLI_SYSTEM_SETTINGS_PATH", &absent_system_settings);

    // Redirect USERPROFILE to an empty temp dir so the developer's real
    // ~/.gemini/settings.json is not consulted either.
    let empty_home = std::env::temp_dir().join(format!("termal-gemini-home-{}", Uuid::new_v4()));
    fs::create_dir_all(&empty_home).expect("empty home dir should be created");
    let _home_env = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &empty_home);

    fs::write(
        &settings_path,
        r#"{"tools":{"shell":{"enableInteractiveShell":true}}}"#,
    )
    .expect("enabled Gemini settings should be written");
    let enabled_warning = gemini_interactive_shell_warning(workdir)
        .expect("enabled interactive shell should warn on Windows");
    assert!(enabled_warning.contains("TermAl forces Gemini"));
    assert!(enabled_warning.contains(&display_path_for_user(&settings_path)));

    fs::write(
        &settings_path,
        r#"{"tools":{"shell":{"enableInteractiveShell":false}}}"#,
    )
    .expect("disabled Gemini settings should be written");
    assert_eq!(gemini_interactive_shell_warning(workdir), None);

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(empty_home);
}

// Tests that Codex Windows warnings point users toward WSL when shell parsing fails upstream.
#[test]
fn codex_windows_shell_warning_matches_platform() {
    if cfg!(windows) {
        let warning = codex_windows_shell_warning()
            .expect("Windows builds should surface the Codex shell warning");
        assert!(warning.contains("WSL"));
    } else {
        assert_eq!(codex_windows_shell_warning(), None);
    }
}

// Tests that Codex readiness reflects runtime CLI detection and warning wiring.
#[test]
fn codex_agent_readiness_matches_runtime_resolution() {
    let readiness = codex_agent_readiness();

    assert!(matches!(readiness.agent, Agent::Codex));
    match readiness.command_path.as_deref() {
        Some(command_path) => {
            assert!(matches!(readiness.status, AgentReadinessStatus::Ready));
            assert!(!readiness.blocking);
            assert!(readiness.detail.contains(command_path));
            assert_eq!(readiness.warning_detail, codex_windows_shell_warning());
        }
        None => {
            assert!(matches!(readiness.status, AgentReadinessStatus::Missing));
            assert!(readiness.blocking);
            assert!(readiness.detail.contains("Install the `codex` CLI"));
            assert_eq!(readiness.warning_detail, None);
        }
    }
}

fn sentinel_agent_readiness_snapshot() -> Vec<AgentReadiness> {
    vec![
        AgentReadiness {
            agent: Agent::Codex,
            status: AgentReadinessStatus::NeedsSetup,
            blocking: true,
            detail: "sentinel codex readiness".to_owned(),
            warning_detail: Some("sentinel codex warning".to_owned()),
            command_path: Some("sentinel-codex".to_owned()),
        },
        AgentReadiness {
            agent: Agent::Cursor,
            status: AgentReadinessStatus::Missing,
            blocking: true,
            detail: "sentinel cursor readiness".to_owned(),
            warning_detail: None,
            command_path: Some("sentinel-cursor".to_owned()),
        },
        AgentReadiness {
            agent: Agent::Gemini,
            status: AgentReadinessStatus::Ready,
            blocking: false,
            detail: "sentinel gemini readiness".to_owned(),
            warning_detail: Some("sentinel gemini warning".to_owned()),
            command_path: Some("sentinel-gemini".to_owned()),
        },
    ]
}

// Tests that hot-path snapshots use the cached readiness value even when the
// cache TTL has expired.  `snapshot_from_inner` deliberately skips refresh
// because it runs under the `inner` mutex where filesystem I/O is unsafe.
#[test]
fn snapshot_from_inner_uses_cached_agent_readiness_when_cache_is_stale() {
    let state = test_app_state();
    let sentinel = sentinel_agent_readiness_snapshot();
    {
        let mut cache = state
            .agent_readiness_cache
            .write()
            .expect("agent readiness cache should not be poisoned");
        // Only expire the TTL — `invalidated` stays false so the test isolates
        // the TTL-stale path without conflating the two staleness signals.
        *cache = AgentReadinessCache {
            snapshot: sentinel.clone(),
            expires_at: Instant::now() - AGENT_READINESS_CACHE_TTL,
            invalidated: false,
        };
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    let snapshot = state.snapshot_from_inner(&inner);
    drop(inner);

    assert_eq!(snapshot.agent_readiness, sentinel);
}

// Tests that app-settings invalidation refreshes readiness before returning a full snapshot.
#[test]
fn update_app_settings_refreshes_invalidated_agent_readiness_cache() {
    let state = test_app_state();
    let sentinel = sentinel_agent_readiness_snapshot();
    {
        let mut cache = state
            .agent_readiness_cache
            .write()
            .expect("agent readiness cache should not be poisoned");
        *cache = AgentReadinessCache::fresh(sentinel.clone());
    }

    let next_reasoning_effort = {
        let current = state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .preferences
            .default_codex_reasoning_effort;
        if current == CodexReasoningEffort::High {
            CodexReasoningEffort::Medium
        } else {
            CodexReasoningEffort::High
        }
    };

    let updated = state
        .update_app_settings(UpdateAppSettingsRequest {
            default_codex_reasoning_effort: Some(next_reasoning_effort),
            default_claude_approval_mode: None,
            default_claude_effort: None,
            remotes: None,
        })
        .expect("app settings should update");

    assert_ne!(updated.agent_readiness, sentinel);

    let cache = state
        .agent_readiness_cache
        .read()
        .expect("agent readiness cache should not be poisoned");
    assert_eq!(cache.snapshot, updated.agent_readiness);
    assert!(!cache.invalidated);
}

// Tests that `snapshot()` refreshes agent readiness when the TTL has expired
// but the cache was not explicitly invalidated.
#[test]
fn snapshot_refreshes_agent_readiness_when_ttl_expires() {
    let state = test_app_state();
    let sentinel = sentinel_agent_readiness_snapshot();
    {
        let mut cache = state
            .agent_readiness_cache
            .write()
            .expect("agent readiness cache should not be poisoned");
        *cache = AgentReadinessCache {
            snapshot: sentinel.clone(),
            expires_at: Instant::now() - AGENT_READINESS_CACHE_TTL,
            invalidated: false,
        };
    }

    let snapshot = state.snapshot();

    // `snapshot()` should have refreshed the cache, producing readiness from a
    // real `collect_agent_readiness` call rather than returning the sentinel.
    assert_ne!(snapshot.agent_readiness, sentinel);

    let cache = state
        .agent_readiness_cache
        .read()
        .expect("agent readiness cache should not be poisoned");
    assert_eq!(cache.snapshot, snapshot.agent_readiness);
    assert!(!cache.invalidated);
}

// Tests that hot-path snapshots use the cached readiness value even when the
// cache has been explicitly invalidated.  Together with the TTL-stale variant
// above, this confirms `snapshot_from_inner` never refreshes under any conditions.
#[test]
fn snapshot_from_inner_uses_cached_agent_readiness_when_cache_is_invalidated() {
    let state = test_app_state();
    let sentinel = sentinel_agent_readiness_snapshot();
    {
        let mut cache = state
            .agent_readiness_cache
            .write()
            .expect("agent readiness cache should not be poisoned");
        *cache = AgentReadinessCache {
            snapshot: sentinel.clone(),
            // TTL still valid, but explicitly invalidated.
            expires_at: Instant::now() + AGENT_READINESS_CACHE_TTL,
            invalidated: true,
        };
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    let snapshot = state.snapshot_from_inner(&inner);
    drop(inner);

    assert_eq!(snapshot.agent_readiness, sentinel);
}

// Tests that `update_app_settings` publishes an SSE event whose revision and
// agent readiness match the returned API response, eliminating the stale-SSE /
// duplicate-revision race.
#[test]
fn update_app_settings_sse_matches_api_response() {
    let state = test_app_state();
    let sentinel = sentinel_agent_readiness_snapshot();
    {
        let mut cache = state
            .agent_readiness_cache
            .write()
            .expect("agent readiness cache should not be poisoned");
        *cache = AgentReadinessCache::fresh(sentinel.clone());
    }

    let mut state_events = state.subscribe_events();
    let next_reasoning_effort = {
        let current = state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .preferences
            .default_codex_reasoning_effort;
        if current == CodexReasoningEffort::High {
            CodexReasoningEffort::Medium
        } else {
            CodexReasoningEffort::High
        }
    };
    let api_response = state
        .update_app_settings(UpdateAppSettingsRequest {
            default_codex_reasoning_effort: Some(next_reasoning_effort),
            default_claude_approval_mode: None,
            default_claude_effort: None,
            remotes: None,
        })
        .expect("app settings should update");

    let published: StateResponse = serde_json::from_str(
        &state_events
            .try_recv()
            .expect("update_app_settings should publish a state snapshot"),
    )
    .expect("SSE state event should decode");

    assert_eq!(published.revision, api_response.revision);
    assert_eq!(published.agent_readiness, api_response.agent_readiness);
}

// Tests that `create_session` refreshes the agent readiness cache so the SSE
// event and API response carry fresh (non-sentinel) readiness.
#[test]
fn create_session_refreshes_agent_readiness_cache() {
    let state = test_app_state();
    let sentinel = sentinel_agent_readiness_snapshot();
    {
        let mut cache = state
            .agent_readiness_cache
            .write()
            .expect("agent readiness cache should not be poisoned");
        *cache = AgentReadinessCache::fresh(sentinel.clone());
    }

    let mut state_events = state.subscribe_events();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Cache Test".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("session should be created");

    // The API response should carry freshly-computed readiness, not the sentinel.
    assert_ne!(created.state.agent_readiness, sentinel);

    // The SSE event should match the API response exactly.
    let published: StateResponse = serde_json::from_str(
        &state_events
            .try_recv()
            .expect("create_session should publish a state snapshot"),
    )
    .expect("SSE state event should decode");
    assert_eq!(published.revision, created.state.revision);
    assert_eq!(published.agent_readiness, created.state.agent_readiness);
}

// Tests that AgentReadiness serializes warning_detail as warningDetail.
#[test]
fn agent_readiness_serialization_emits_warning_detail_camel_case() {
    let readiness = AgentReadiness {
        agent: Agent::Codex,
        status: AgentReadinessStatus::Ready,
        blocking: false,
        detail: "Codex CLI is available.".to_owned(),
        warning_detail: Some("Use WSL for shell commands on Windows.".to_owned()),
        command_path: Some("codex".to_owned()),
    };

    let serialized =
        serde_json::to_value(&readiness).expect("AgentReadiness should serialize to JSON");
    assert_eq!(
        serialized.pointer("/warningDetail"),
        Some(&Value::String(
            "Use WSL for shell commands on Windows.".to_owned()
        ))
    );

    let serialized_without_warning = serde_json::to_value(AgentReadiness {
        warning_detail: None,
        ..readiness
    })
    .expect("AgentReadiness without warning detail should serialize to JSON");
    assert_eq!(serialized_without_warning.get("warningDetail"), None);
}
// Tests that Gemini falls back from a rejected session/load to a new ACP session.
#[test]
fn gemini_invalid_session_load_falls_back_to_session_new() {
    // Hold the home-env mutex and set a dummy GEMINI_API_KEY so
    // validate_agent_session_setup passes on machines without real Gemini
    // credentials.  The API key is never used for a real network call — the
    // ACP runtime is driven by SharedBufferWriter throughout the test.
    let _env_lock = TEST_HOME_ENV_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _api_key = ScopedEnvVar::set("GEMINI_API_KEY", "test-key-not-real");

    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Gemini),
            name: Some("Gemini Resume".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gemini-pro".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: Some(default_gemini_approval_mode()),
        })
        .expect("Gemini session should be created");
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: None,
        is_loading_history: false,
        supports_session_load: Some(true),
    }));
    let writer = SharedBufferWriter::default();
    let thread_writer = writer.clone();
    let thread_pending_requests = pending_requests.clone();
    let thread_state = state.clone();
    let thread_runtime_state = runtime_state.clone();
    let thread_session_id = created.session_id.clone();
    let handle = std::thread::spawn(move || {
        let mut stdin = thread_writer;
        ensure_acp_session_ready(
            &mut stdin,
            &thread_pending_requests,
            &thread_state,
            &thread_session_id,
            &thread_runtime_state,
            AcpAgent::Gemini,
            &AcpPromptCommand {
                cwd: "/tmp".to_owned(),
                cursor_mode: None,
                model: "gemini-pro".to_owned(),
                prompt: "Resume the prior session".to_owned(),
                resume_session_id: Some("gemini-session-stale".to_owned()),
            },
        )
    });

    let (_load_request_id, load_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    load_sender
        .send(Err(AcpResponseError::JsonRpc(AcpJsonRpcError {
            code: Some(-32602),
            message: "Invalid session identifier".to_owned(),
            data: Some(json!({
                "reason": "invalidSessionIdentifier",
            })),
        })))
        .expect("session/load response should send");

    let (_new_request_id, new_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    new_sender
        .send(Ok(json!({
            "sessionId": "gemini-session-new",
            "configOptions": [
                {
                    "id": "model",
                    "currentValue": "gemini-pro",
                    "options": [
                        {
                            "value": "gemini-pro",
                            "name": "Gemini Pro"
                        }
                    ]
                }
            ]
        })))
        .expect("session/new response should send");

    let external_session_id = handle
        .join()
        .expect("Gemini ACP worker should finish")
        .expect("Gemini fallback should recover with a new session");
    assert_eq!(external_session_id, "gemini-session-new");
    let written = writer.contents();
    let load_index = written
        .find("\"method\":\"session/load\"")
        .expect("session/load request should be written");
    let new_index = written
        .find("\"method\":\"session/new\"")
        .expect("session/new request should be written");
    assert!(
        load_index < new_index,
        "session/load should happen before session/new\n{written}"
    );
    let session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Gemini session should be present");
    assert_eq!(
        session.external_session_id.as_deref(),
        Some("gemini-session-new")
    );
    assert_eq!(
        runtime_state
            .lock()
            .expect("ACP runtime state mutex poisoned")
            .current_session_id
            .as_deref(),
        Some("gemini-session-new")
    );
}

// Tests that shared Codex global notices update Codex state.
#[test]
fn shared_codex_global_notices_update_codex_state() {
    let state = test_app_state();
    let (runtime, _input_rx, _process) = test_shared_codex_runtime("shared-codex-global-notices");
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));

    let config_warning = json!({
        "method": "configWarning",
        "params": {
            "message": "Codex is using fallback sandbox defaults.",
            "code": "sandbox_fallback"
        }
    });
    let deprecation_notice = json!({
        "method": "deprecationNotice",
        "params": {
            "title": "Legacy model alias",
            "detail": "`gpt-4` will be removed soon.",
            "code": "legacy_model_alias"
        }
    });

    handle_shared_codex_app_server_message(
        &config_warning,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &config_warning,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &deprecation_notice,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let codex = state.snapshot().codex;
    assert_eq!(codex.notices.len(), 2);
    assert!(matches!(
        codex.notices.first(),
        Some(CodexNotice {
            kind: CodexNoticeKind::DeprecationNotice,
            level: CodexNoticeLevel::Info,
            title,
            detail,
            code,
            ..
        }) if title == "Legacy model alias"
            && detail == "`gpt-4` will be removed soon."
            && code.as_deref() == Some("legacy_model_alias")
    ));
    assert!(matches!(
        codex.notices.get(1),
        Some(CodexNotice {
            kind: CodexNoticeKind::ConfigWarning,
            level: CodexNoticeLevel::Warning,
            title,
            detail,
            code,
            ..
        }) if title == "Config warning"
            && detail == "Codex is using fallback sandbox defaults."
            && code.as_deref() == Some("sandbox_fallback")
    ));
}

// Tests that shared Codex threadless runtime notice is recorded.
#[test]
fn shared_codex_threadless_runtime_notice_is_recorded() {
    let state = test_app_state();
    let (runtime, _input_rx, _process) = test_shared_codex_runtime("shared-codex-runtime-notice");
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_notice = json!({
        "method": "authRequired",
        "params": {
            "message": "Sign in again before continuing.",
            "code": "auth_required",
            "level": "warning"
        }
    });

    handle_shared_codex_app_server_message(
        &runtime_notice,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let codex = state.snapshot().codex;
    assert!(matches!(
        codex.notices.first(),
        Some(CodexNotice {
            kind: CodexNoticeKind::RuntimeNotice,
            level: CodexNoticeLevel::Warning,
            title,
            detail,
            code,
            ..
        }) if title == "Codex notice: authRequired"
            && detail == "Sign in again before continuing."
            && code.as_deref() == Some("auth_required")
    ));
}

// Tests that discover Codex threads from home reads latest database.
#[test]
fn discover_codex_threads_from_home_reads_latest_database() {
    let codex_home = std::env::temp_dir().join(format!("termal-codex-home-{}", Uuid::new_v4()));
    fs::write(codex_home.join("state.db"), b"").unwrap_or_default();
    write_test_codex_threads_db(
        &codex_home,
        &[(
            "thread-1",
            "/tmp/project",
            "Review local repo",
            r#"{"type":"danger-full-access"}"#,
            "on-request",
            1,
            Some("gpt-5-codex"),
            Some("high"),
            10,
        )],
    );

    let threads = discover_codex_threads_from_home(&codex_home, &[PathBuf::from("/tmp/project")])
        .expect("threads should load");

    assert_eq!(
        threads,
        vec![DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::OnRequest),
            archived: true,
            cwd: "/tmp/project".to_owned(),
            id: "thread-1".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: Some(CodexReasoningEffort::High),
            sandbox_mode: Some(CodexSandboxMode::DangerFullAccess),
            title: "Review local repo".to_owned(),
        }]
    );

    let _ = fs::remove_dir_all(&codex_home);
}

// Tests that discover Codex threads from home requires optional columns.
#[test]
fn discover_codex_threads_from_home_requires_optional_columns() {
    let codex_home =
        std::env::temp_dir().join(format!("termal-codex-home-legacy-{}", Uuid::new_v4()));
    fs::create_dir_all(&codex_home).expect("test Codex home should be created");
    let connection =
        rusqlite::Connection::open(codex_home.join("state_5.sqlite")).expect("db should open");
    connection
        .execute_batch(
            "create table threads (
                id text primary key,
                cwd text not null,
                title text not null,
                sandbox_policy text,
                approval_mode text,
                archived integer not null,
                updated_at integer not null
            );",
        )
        .expect("legacy threads table should be created");
    connection
        .execute(
            "insert into threads (
                id, cwd, title, sandbox_policy, approval_mode, archived, updated_at
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                "thread-legacy",
                "/tmp/project",
                "Legacy thread",
                r#"{"type":"workspace-write"}"#,
                "on-request",
                0,
                10,
            ],
        )
        .expect("legacy thread row should insert");

    let err = discover_codex_threads_from_home(&codex_home, &[PathBuf::from("/tmp/project")])
        .expect_err("legacy threads schema should fail");
    let err_text = format!("{err:#}");
    assert!(
        err_text.contains("no such column"),
        "unexpected discovery error: {err_text}"
    );

    let _ = fs::remove_dir_all(&codex_home);
}

// Tests that resolve Codex threads database path skips unrelated entries.
#[test]
fn resolve_codex_threads_database_path_skips_unrelated_entries() {
    let codex_home =
        std::env::temp_dir().join(format!("termal-codex-home-scan-{}", Uuid::new_v4()));
    fs::create_dir_all(&codex_home).expect("test Codex home should be created");
    fs::write(codex_home.join("state_9.sqlite"), b"sqlite").expect("valid state db should exist");
    fs::write(codex_home.join("state_preview.sqlite"), b"broken")
        .expect("unrelated sqlite file should be created");

    let path = resolve_codex_threads_database_path(&codex_home)
        .expect("database discovery should skip unrelated entries");

    assert_eq!(
        path.file_name().and_then(|value| value.to_str()),
        Some("state_9.sqlite")
    );

    let _ = fs::remove_dir_all(&codex_home);
}

// Tests that discover Codex threads from sources skips REPL home and uses shared runtime home.
#[test]
fn discover_codex_threads_from_sources_skips_repl_home_and_uses_shared_runtime_home() {
    let root = std::env::temp_dir().join(format!("termal-codex-discovery-{}", Uuid::new_v4()));
    let source_home = root.join(".codex");
    let termal_root = root.join(".termal").join("codex-home");
    let shared_home = termal_root.join("shared-app-server");
    let repl_home = termal_root.join("repl");

    write_test_codex_threads_db(
        &shared_home,
        &[(
            "thread-shared",
            "/tmp/project-shared",
            "Shared runtime thread",
            r#"{"type":"workspace-write"}"#,
            "on-request",
            0,
            Some("gpt-5-codex"),
            Some("medium"),
            30,
        )],
    );
    write_test_codex_threads_db(
        &repl_home,
        &[(
            "thread-repl",
            "/tmp/project-repl",
            "REPL thread",
            r#"{"type":"read-only"}"#,
            "never",
            0,
            Some("gpt-5-mini"),
            Some("low"),
            20,
        )],
    );
    write_test_codex_threads_db(
        &source_home,
        &[
            (
                "thread-shared",
                "/tmp/project-source",
                "Older source copy",
                r#"{"type":"danger-full-access"}"#,
                "never",
                1,
                Some("gpt-5"),
                Some("high"),
                10,
            ),
            (
                "thread-source",
                "/tmp/project-source-only",
                "Source-only thread",
                r#"{"type":"workspace-write"}"#,
                "on-failure",
                0,
                Some("gpt-5-codex"),
                Some("minimal"),
                5,
            ),
        ],
    );

    let threads = discover_codex_threads_from_sources(
        Some(&source_home),
        &termal_root,
        &[
            PathBuf::from("/tmp/project-shared"),
            PathBuf::from("/tmp/project-source-only"),
        ],
    )
    .expect("threads should load");

    assert_eq!(
        threads
            .iter()
            .map(|thread| thread.id.as_str())
            .collect::<Vec<_>>(),
        vec!["thread-shared", "thread-source"]
    );
    assert!(matches!(
        threads.first(),
        Some(DiscoveredCodexThread {
            title,
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            ..
        }) if title == "Shared runtime thread"
    ));
    assert!(threads.iter().all(|thread| thread.id != "thread-repl"));

    let _ = fs::remove_dir_all(&root);
}

// Tests that discover Codex threads from home filters scopes before limiting results.
#[test]
fn discover_codex_threads_from_home_filters_scopes_before_limiting_results() {
    let codex_home =
        std::env::temp_dir().join(format!("termal-codex-home-large-{}", Uuid::new_v4()));
    fs::create_dir_all(&codex_home).expect("test Codex home should be created");
    let connection =
        rusqlite::Connection::open(codex_home.join("state_5.sqlite")).expect("db should open");
    connection
        .execute_batch(
            "create table threads (
                id text primary key,
                cwd text not null,
                title text not null,
                sandbox_policy text not null,
                approval_mode text not null,
                archived integer not null,
                model text,
                reasoning_effort text,
                updated_at integer not null
            );",
        )
        .expect("threads table should be created");

    for index in 0..101 {
        connection
            .execute(
                "insert into threads (
                    id, cwd, title, sandbox_policy, approval_mode, archived, model, reasoning_effort, updated_at
                ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    format!("thread-other-{index}"),
                    "/tmp/out-of-scope",
                    format!("Out-of-scope thread {index}"),
                    r#"{"type":"workspace-write"}"#,
                    "never",
                    0,
                    "gpt-5-codex",
                    "low",
                    1_000 - index,
                ],
            )
            .expect("thread row should insert");
    }
    connection
        .execute(
            "insert into threads (
                id, cwd, title, sandbox_policy, approval_mode, archived, model, reasoning_effort, updated_at
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                "thread-target",
                "/tmp/termal",
                "Older in-scope thread",
                r#"{"type":"danger-full-access"}"#,
                "on-request",
                0,
                "gpt-5-codex",
                "medium",
                1,
            ],
        )
        .expect("target row should insert");

    let threads = discover_codex_threads_from_home(&codex_home, &[PathBuf::from("/tmp/termal")])
        .expect("threads should load");

    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].id, "thread-target");

    let _ = fs::remove_dir_all(&codex_home);
}

// Tests that discover Codex threads from home limits in scope results per home.
#[test]
fn discover_codex_threads_from_home_limits_in_scope_results_per_home() {
    let codex_home =
        std::env::temp_dir().join(format!("termal-codex-home-limited-{}", Uuid::new_v4()));
    fs::create_dir_all(&codex_home).expect("test Codex home should be created");
    let connection =
        rusqlite::Connection::open(codex_home.join("state_7.sqlite")).expect("db should open");
    connection
        .execute_batch(
            "create table threads (
                id text primary key,
                cwd text not null,
                title text not null,
                sandbox_policy text not null,
                approval_mode text not null,
                archived integer not null,
                model text,
                reasoning_effort text,
                updated_at integer not null
            );",
        )
        .expect("threads table should be created");

    for index in 0..(MAX_DISCOVERED_CODEX_THREADS_PER_HOME + 25) {
        connection
            .execute(
                "insert into threads (
                    id, cwd, title, sandbox_policy, approval_mode, archived, model, reasoning_effort, updated_at
                ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    format!("thread-in-scope-{index}"),
                    "/tmp/termal/subdir",
                    format!("In-scope thread {index}"),
                    r#"{"type":"workspace-write"}"#,
                    "on-request",
                    0,
                    "gpt-5-codex",
                    "medium",
                    10_000 - index as i64,
                ],
            )
            .expect("thread row should insert");
    }

    let threads = discover_codex_threads_from_home(&codex_home, &[PathBuf::from("/tmp/termal")])
        .expect("threads should load");
    let last_expected_id = format!(
        "thread-in-scope-{}",
        MAX_DISCOVERED_CODEX_THREADS_PER_HOME - 1
    );

    assert_eq!(threads.len(), MAX_DISCOVERED_CODEX_THREADS_PER_HOME);
    assert_eq!(
        threads.first().map(|thread| thread.id.as_str()),
        Some("thread-in-scope-0")
    );
    assert_eq!(
        threads.last().map(|thread| thread.id.as_str()),
        Some(last_expected_id.as_str()),
    );

    let _ = fs::remove_dir_all(&codex_home);
}

// Tests that import discovered Codex threads adds project scoped sessions without duplicates.
#[test]
fn import_discovered_codex_threads_adds_project_scoped_sessions_without_duplicates() {
    let mut inner = StateInner::new();
    let project = inner.create_project(
        Some("TermAl".to_owned()),
        "/tmp/termal".to_owned(),
        default_local_remote_id(),
    );
    inner.create_session(
        Agent::Codex,
        Some("Codex Live".to_owned()),
        "/tmp/termal".to_owned(),
        Some(project.id.clone()),
        None,
    );

    let discovered = vec![
        DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::Never),
            archived: true,
            cwd: "/tmp/termal".to_owned(),
            id: "thread-local".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: Some(CodexReasoningEffort::Low),
            sandbox_mode: Some(CodexSandboxMode::DangerFullAccess),
            title: "Read bugs".to_owned(),
        },
        DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::Never),
            archived: false,
            cwd: "/tmp/elsewhere".to_owned(),
            id: "thread-other".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: None,
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            title: "Ignore me".to_owned(),
        },
    ];

    inner.import_discovered_codex_threads("/tmp/termal", discovered.clone());
    inner.import_discovered_codex_threads("/tmp/termal", discovered);

    let discovered_session = inner
        .sessions
        .iter()
        .find(|record| record.external_session_id.as_deref() == Some("thread-local"))
        .expect("project-scoped discovered thread should be imported");
    assert_eq!(discovered_session.session.agent, Agent::Codex);
    assert_eq!(discovered_session.session.workdir, "/tmp/termal");
    assert_eq!(
        discovered_session.session.project_id.as_deref(),
        Some(project.id.as_str())
    );
    assert_eq!(discovered_session.session.model, "gpt-5-codex");
    assert_eq!(
        discovered_session.session.codex_thread_state,
        Some(CodexThreadState::Archived)
    );
    assert_eq!(
        discovered_session.session.preview,
        "Archived Codex thread ready to reopen."
    );
    assert_eq!(
        discovered_session.session.reasoning_effort,
        Some(CodexReasoningEffort::Low)
    );
    assert_eq!(
        discovered_session.session.sandbox_mode,
        Some(CodexSandboxMode::DangerFullAccess)
    );
    assert_eq!(
        discovered_session.session.approval_policy,
        Some(CodexApprovalPolicy::Never)
    );
    assert_eq!(
        inner
            .sessions
            .iter()
            .filter(|record| record.external_session_id.as_deref() == Some("thread-local"))
            .count(),
        1
    );
    assert!(
        inner
            .sessions
            .iter()
            .all(|record| record.external_session_id.as_deref() != Some("thread-other"))
    );
}

// Tests that import discovered Codex threads normalizes legacy local verbatim paths.
#[cfg(windows)]
#[test]
fn import_discovered_codex_threads_normalizes_legacy_local_verbatim_paths() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-discovered-verbatim-path-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let legacy_root = format!(r"\\?\{normalized_root}");

    let mut inner = StateInner::new();
    let project = inner.create_project(
        Some("TermAl".to_owned()),
        normalized_root.clone(),
        default_local_remote_id(),
    );
    inner.import_discovered_codex_threads(
        &normalized_root,
        vec![DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::Never),
            archived: false,
            cwd: legacy_root,
            id: "thread-legacy".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: None,
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            title: "Legacy thread".to_owned(),
        }],
    );

    assert_eq!(inner.projects.len(), 1);
    assert_eq!(inner.projects[0].root_path, normalized_root);
    let record = inner
        .sessions
        .iter()
        .find(|entry| entry.external_session_id.as_deref() == Some("thread-legacy"))
        .expect("legacy discovered thread should be imported");
    assert_eq!(record.session.workdir, normalized_root);
    assert_eq!(
        record.session.project_id.as_deref(),
        Some(project.id.as_str())
    );

    let _ = fs::remove_dir_all(project_root);
}

// Tests that disable_socket_inheritance clears the Windows inherit flag.
#[cfg(windows)]
#[tokio::test]
async fn disable_socket_inheritance_clears_windows_inherit_flag() {
    use std::os::windows::io::AsRawSocket as _;

    unsafe extern "system" {
        fn GetHandleInformation(handle: *mut std::ffi::c_void, flags: *mut u32) -> i32;
        fn SetHandleInformation(handle: *mut std::ffi::c_void, mask: u32, flags: u32) -> i32;
    }

    const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener should bind");
    let raw = listener.as_raw_socket() as *mut std::ffi::c_void;

    let inherited = unsafe { SetHandleInformation(raw, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
    assert_ne!(
        inherited,
        0,
        "test setup should make the socket inheritable: {}",
        io::Error::last_os_error()
    );

    let mut flags = 0u32;
    let queried = unsafe { GetHandleInformation(raw, &mut flags) };
    assert_ne!(
        queried,
        0,
        "test setup should read socket handle flags: {}",
        io::Error::last_os_error()
    );
    assert_ne!(
        flags & HANDLE_FLAG_INHERIT,
        0,
        "test setup should confirm the inherit bit is set"
    );

    disable_socket_inheritance(&listener);

    flags = 0;
    let queried = unsafe { GetHandleInformation(raw, &mut flags) };
    assert_ne!(
        queried,
        0,
        "socket handle flags should remain queryable after inheritance is disabled: {}",
        io::Error::last_os_error()
    );
    assert_eq!(
        flags & HANDLE_FLAG_INHERIT,
        0,
        "disable_socket_inheritance should clear HANDLE_FLAG_INHERIT"
    );
}

// Tests that import discovered Codex threads preserves existing prompt settings.
#[test]
fn import_discovered_codex_threads_preserves_existing_prompt_settings() {
    let mut inner = StateInner::new();
    let project = inner.create_project(
        Some("TermAl".to_owned()),
        "/tmp/termal".to_owned(),
        default_local_remote_id(),
    );
    let mut record = inner.create_session(
        Agent::Codex,
        Some("Codex Live".to_owned()),
        "/tmp/termal".to_owned(),
        Some(project.id.clone()),
        Some("gpt-5-mini".to_owned()),
    );
    record.codex_sandbox_mode = CodexSandboxMode::ReadOnly;
    record.session.sandbox_mode = Some(CodexSandboxMode::ReadOnly);
    record.codex_approval_policy = CodexApprovalPolicy::OnFailure;
    record.session.approval_policy = Some(CodexApprovalPolicy::OnFailure);
    record.codex_reasoning_effort = CodexReasoningEffort::Minimal;
    record.session.reasoning_effort = Some(CodexReasoningEffort::Minimal);
    set_record_external_session_id(&mut record, Some("thread-existing".to_owned()));
    if let Some(slot) = inner
        .find_session_index(&record.session.id)
        .and_then(|index| inner.sessions.get_mut(index))
    {
        *slot = record;
    }

    inner.import_discovered_codex_threads(
        "/tmp/termal",
        vec![DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::Never),
            archived: true,
            cwd: "/tmp/termal".to_owned(),
            id: "thread-existing".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: Some(CodexReasoningEffort::High),
            sandbox_mode: Some(CodexSandboxMode::DangerFullAccess),
            title: "Existing thread".to_owned(),
        }],
    );

    let record = inner
        .sessions
        .iter()
        .find(|entry| entry.external_session_id.as_deref() == Some("thread-existing"))
        .expect("existing discovered thread should still be present");
    assert_eq!(record.session.model, "gpt-5-mini");
    assert_eq!(
        record.session.sandbox_mode,
        Some(CodexSandboxMode::ReadOnly)
    );
    assert_eq!(
        record.session.approval_policy,
        Some(CodexApprovalPolicy::OnFailure)
    );
    assert_eq!(
        record.session.reasoning_effort,
        Some(CodexReasoningEffort::Minimal)
    );
    assert_eq!(
        record.session.codex_thread_state,
        Some(CodexThreadState::Archived)
    );
}

// Tests that create session route returns created response.
#[tokio::test]
async fn create_session_route_returns_created_response() {
    let state = test_app_state();
    let initial_session_count = state.snapshot().sessions.len();
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "name": "Route Created Session",
        "workdir": "/tmp"
    }))
    .expect("create session route body should serialize");
    let (status, response): (StatusCode, CreateSessionResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/sessions")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(response.state.sessions.len(), initial_session_count + 1);
    let created_session = response
        .state
        .sessions
        .iter()
        .find(|session| session.id == response.session_id)
        .expect("created session should be present");
    assert_eq!(created_session.name, "Route Created Session");
    let expected_workdir = resolve_session_workdir("/tmp").expect("route workdir should normalize");
    assert_eq!(created_session.workdir, expected_workdir);
    assert_eq!(created_session.agent, Agent::Codex);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that workspace layout routes round-trip put, get, and list calls.
#[tokio::test]
async fn workspace_layout_routes_round_trip_put_get_and_list() {
    let state = test_app_state();
    let app = app_router(state.clone());
    let initial_workspace = json!({
        "panes": [
            {
                "id": "pane-1",
                "kind": "session",
                "sessionId": "session-1"
            }
        ]
    });
    let initial_body = serde_json::to_vec(&json!({
        "controlPanelSide": "left",
        "themeId": "terminal",
        "styleId": "style-terminal",
        "fontSizePx": 14,
        "editorFontSizePx": 15,
        "densityPercent": 90,
        "workspace": initial_workspace.clone()
    }))
    .expect("workspace layout body should serialize");
    let (create_status, create_response): (StatusCode, WorkspaceLayoutResponse) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/workspaces/workspace-1")
            .header("content-type", "application/json")
            .body(Body::from(initial_body))
            .unwrap(),
    )
    .await;

    assert_eq!(create_status, StatusCode::OK);
    assert_eq!(create_response.layout.id, "workspace-1");
    assert_eq!(create_response.layout.revision, 1);
    assert_eq!(
        create_response.layout.control_panel_side,
        WorkspaceControlPanelSide::Left
    );
    assert_eq!(create_response.layout.theme_id.as_deref(), Some("terminal"));
    assert_eq!(
        create_response.layout.style_id.as_deref(),
        Some("style-terminal")
    );
    assert_eq!(create_response.layout.font_size_px, Some(14));
    assert_eq!(create_response.layout.editor_font_size_px, Some(15));
    assert_eq!(create_response.layout.density_percent, Some(90));
    assert_eq!(create_response.layout.workspace, initial_workspace);
    assert!(!create_response.layout.updated_at.is_empty());

    let updated_workspace = json!({
        "activePaneId": "pane-2",
        "panes": [
            {
                "id": "pane-2",
                "kind": "source",
                "sourcePath": "src/lib.rs"
            }
        ]
    });
    let update_body = serde_json::to_vec(&json!({
        "controlPanelSide": "right",
        "themeId": "frost",
        "styleId": "style-editorial",
        "fontSizePx": 16,
        "editorFontSizePx": 17,
        "densityPercent": 110,
        "workspace": updated_workspace.clone()
    }))
    .expect("updated workspace layout body should serialize");
    let (update_status, update_response): (StatusCode, WorkspaceLayoutResponse) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/workspaces/workspace-1")
            .header("content-type", "application/json")
            .body(Body::from(update_body))
            .unwrap(),
    )
    .await;

    assert_eq!(update_status, StatusCode::OK);
    assert_eq!(update_response.layout.id, "workspace-1");
    assert_eq!(update_response.layout.revision, 2);
    assert_eq!(
        update_response.layout.control_panel_side,
        WorkspaceControlPanelSide::Right
    );
    assert_eq!(update_response.layout.theme_id.as_deref(), Some("frost"));
    assert_eq!(
        update_response.layout.style_id.as_deref(),
        Some("style-editorial")
    );
    assert_eq!(update_response.layout.font_size_px, Some(16));
    assert_eq!(update_response.layout.editor_font_size_px, Some(17));
    assert_eq!(update_response.layout.density_percent, Some(110));
    assert_eq!(update_response.layout.workspace, updated_workspace.clone());
    assert!(!update_response.layout.updated_at.is_empty());

    let (get_status, get_response): (StatusCode, WorkspaceLayoutResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/workspaces/workspace-1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(get_status, StatusCode::OK);
    assert_eq!(get_response.layout.id, "workspace-1");
    assert_eq!(get_response.layout.revision, 2);
    assert_eq!(
        get_response.layout.control_panel_side,
        WorkspaceControlPanelSide::Right
    );
    assert_eq!(get_response.layout.theme_id.as_deref(), Some("frost"));
    assert_eq!(
        get_response.layout.style_id.as_deref(),
        Some("style-editorial")
    );
    assert_eq!(get_response.layout.font_size_px, Some(16));
    assert_eq!(get_response.layout.editor_font_size_px, Some(17));
    assert_eq!(get_response.layout.density_percent, Some(110));
    assert_eq!(get_response.layout.workspace, updated_workspace);
    assert!(!get_response.layout.updated_at.is_empty());
    assert_eq!(
        get_response.layout.updated_at,
        update_response.layout.updated_at
    );

    let (list_status, list_response): (StatusCode, WorkspaceLayoutsResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/workspaces")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(list_status, StatusCode::OK);
    assert_eq!(list_response.workspaces.len(), 1);
    let summary = &list_response.workspaces[0];
    assert_eq!(summary.id, "workspace-1");
    assert_eq!(summary.revision, 2);
    assert_eq!(summary.control_panel_side, WorkspaceControlPanelSide::Right);
    assert_eq!(summary.theme_id.as_deref(), Some("frost"));
    assert_eq!(summary.style_id.as_deref(), Some("style-editorial"));
    assert_eq!(summary.font_size_px, Some(16));
    assert_eq!(summary.editor_font_size_px, Some(17));
    assert_eq!(summary.density_percent, Some(110));
    assert!(!summary.updated_at.is_empty());
    assert_eq!(summary.updated_at, get_response.layout.updated_at);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that workspace layout list route orders newer documents first.
#[tokio::test]
async fn workspace_layout_list_route_orders_workspaces_by_updated_at_desc() {
    let state = test_app_state();
    let app = app_router(state.clone());
    let layout_body = |side: &str| {
        serde_json::to_vec(&json!({
            "controlPanelSide": side,
            "workspace": { "panes": [] }
        }))
        .expect("workspace layout body should serialize")
    };

    for workspace_id in ["workspace-b", "workspace-a", "workspace-c"] {
        let (status, _response): (StatusCode, WorkspaceLayoutResponse) = request_json(
            &app,
            Request::builder()
                .method("PUT")
                .uri(format!("/api/workspaces/{workspace_id}"))
                .header("content-type", "application/json")
                .body(Body::from(layout_body("left")))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .workspace_layouts
            .get_mut("workspace-b")
            .expect("workspace-b should exist")
            .updated_at = "2026-04-02 08:30:00".to_owned();
        inner
            .workspace_layouts
            .get_mut("workspace-a")
            .expect("workspace-a should exist")
            .updated_at = "2026-04-02 08:30:00".to_owned();
        inner
            .workspace_layouts
            .get_mut("workspace-c")
            .expect("workspace-c should exist")
            .updated_at = "2026-04-03 09:45:00".to_owned();
    }

    let (status, response): (StatusCode, WorkspaceLayoutsResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/workspaces")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    // Matching timestamps fall back to ascending workspace ID, so the tied
    // 2026-04-02 entries must appear as `workspace-a` before `workspace-b`.
    assert_eq!(
        response
            .workspaces
            .iter()
            .map(|workspace| workspace.id.as_str())
            .collect::<Vec<_>>(),
        vec!["workspace-c", "workspace-a", "workspace-b"]
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that delete workspace layout route removes a saved workspace and returns the remaining summaries.
#[tokio::test]
async fn delete_workspace_layout_route_removes_saved_workspace() {
    let state = test_app_state();
    let app = app_router(state.clone());
    let layout_body = serde_json::to_vec(&json!({
        "controlPanelSide": "left",
        "workspace": { "panes": [] }
    }))
    .expect("workspace layout body should serialize");

    for workspace_id in ["workspace-1", "workspace-2"] {
        let (status, _response): (StatusCode, WorkspaceLayoutResponse) = request_json(
            &app,
            Request::builder()
                .method("PUT")
                .uri(format!("/api/workspaces/{workspace_id}"))
                .header("content-type", "application/json")
                .body(Body::from(layout_body.clone()))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    let (delete_status, delete_response): (StatusCode, WorkspaceLayoutsResponse) = request_json(
        &app,
        Request::builder()
            .method("DELETE")
            .uri("/api/workspaces/workspace-1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(delete_status, StatusCode::OK);
    assert_eq!(delete_response.workspaces.len(), 1);
    assert_eq!(delete_response.workspaces[0].id, "workspace-2");

    let (get_status, get_error): (StatusCode, ErrorResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/workspaces/workspace-1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(get_status, StatusCode::NOT_FOUND);
    assert_eq!(get_error.error, "workspace layout not found");
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that delete workspace layout route returns not found for missing IDs.
#[tokio::test]
async fn delete_workspace_layout_route_returns_not_found_for_missing_id() {
    let state = test_app_state();
    let app = app_router(state.clone());
    let (status, error): (StatusCode, ErrorResponse) = request_json(
        &app,
        Request::builder()
            .method("DELETE")
            .uri("/api/workspaces/missing-workspace")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error.error, "workspace layout not found");
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that get workspace layout route returns not found for missing IDs.
#[tokio::test]
async fn get_workspace_layout_route_returns_not_found_for_missing_id() {
    let state = test_app_state();
    let app = app_router(state.clone());
    let (status, error): (StatusCode, ErrorResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/workspaces/missing-workspace")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error.error, "workspace layout not found");
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that put workspace layout route rejects malformed payloads.
#[tokio::test]
async fn put_workspace_layout_route_rejects_malformed_payloads() {
    let state = test_app_state();
    let app = app_router(state.clone());
    let missing_control_panel_response = request_response(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/workspaces/workspace-1")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "workspace": {} }))
                    .expect("missing-control-panel workspace body should serialize"),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(
        missing_control_panel_response.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let missing_control_panel_body =
        to_bytes(missing_control_panel_response.into_body(), usize::MAX)
            .await
            .expect("missing-control-panel rejection body should read");
    let missing_control_panel_text = String::from_utf8(missing_control_panel_body.to_vec())
        .expect("missing-control-panel rejection body should be UTF-8");
    assert!(missing_control_panel_text.contains("missing field"));
    assert!(missing_control_panel_text.contains("controlPanelSide"));

    let missing_workspace_response = request_response(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/workspaces/workspace-1")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "controlPanelSide": "left" }))
                    .expect("missing-workspace workspace body should serialize"),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(
        missing_workspace_response.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let missing_workspace_body = to_bytes(missing_workspace_response.into_body(), usize::MAX)
        .await
        .expect("missing-workspace rejection body should read");
    let missing_workspace_text = String::from_utf8(missing_workspace_body.to_vec())
        .expect("missing-workspace rejection body should be UTF-8");
    assert!(missing_workspace_text.contains("missing field"));
    assert!(missing_workspace_text.contains("missing field `workspace`"));

    let invalid_enum_response = request_response(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/workspaces/workspace-1")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "controlPanelSide": "middle",
                    "workspace": {}
                }))
                .expect("invalid-enum workspace body should serialize"),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(
        invalid_enum_response.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let invalid_enum_body = to_bytes(invalid_enum_response.into_body(), usize::MAX)
        .await
        .expect("invalid-enum rejection body should read");
    let invalid_enum_text = String::from_utf8(invalid_enum_body.to_vec())
        .expect("invalid-enum rejection body should be UTF-8");
    assert!(invalid_enum_text.contains("unknown variant"));
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that updating an existing workspace layout advances the global revision and publishes state.
#[test]
fn updating_existing_workspace_layout_advances_global_revision_and_publishes_state() {
    let state = test_app_state();
    state
        .put_workspace_layout(
            "workspace-1",
            PutWorkspaceLayoutRequest {
                control_panel_side: WorkspaceControlPanelSide::Left,
                theme_id: None,
                style_id: None,
                font_size_px: None,
                editor_font_size_px: None,
                density_percent: None,
                workspace: json!({ "panes": [] }),
            },
        )
        .expect("initial workspace layout should save");

    let revision_after_create = state.inner.lock().expect("state mutex poisoned").revision;
    let mut state_events = state.subscribe_events();
    let updated = state
        .put_workspace_layout(
            "workspace-1",
            PutWorkspaceLayoutRequest {
                control_panel_side: WorkspaceControlPanelSide::Right,
                theme_id: Some("ink".to_owned()),
                style_id: None,
                font_size_px: None,
                editor_font_size_px: None,
                density_percent: None,
                workspace: json!({
                    "panes": [
                        {
                            "id": "pane-1",
                            "tabs": []
                        }
                    ]
                }),
            },
        )
        .expect("updated workspace layout should save");

    let published_revision = state.inner.lock().expect("state mutex poisoned").revision;
    assert_eq!(updated.layout.revision, 2);
    assert_eq!(published_revision, revision_after_create + 1);

    let published_state: StateResponse = serde_json::from_str(
        &state_events
            .try_recv()
            .expect("workspace update should publish a state snapshot"),
    )
    .expect("state event should decode");
    assert_eq!(published_state.revision, published_revision);
    assert_eq!(published_state.workspaces.len(), 1);
    assert_eq!(published_state.workspaces[0].id, "workspace-1");
    assert_eq!(published_state.workspaces[0].revision, 2);
    assert_eq!(
        published_state.workspaces[0].control_panel_side,
        WorkspaceControlPanelSide::Right
    );
    assert_eq!(
        state
            .get_workspace_layout("workspace-1")
            .expect("saved workspace layout should be readable")
            .layout
            .revision,
        2
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that review persistence replaces the target without leaving temp files behind.
#[test]
fn persist_review_document_replaces_target_without_leaving_temp_files() {
    let review_root =
        std::env::temp_dir().join(format!("termal-review-atomic-write-{}", Uuid::new_v4()));
    fs::create_dir_all(&review_root).expect("review root should exist");
    let change_set_id = "change-set-atomic-write";
    let review_path = resolve_review_document_path(&review_root, change_set_id)
        .expect("review path should resolve");
    let initial_review = default_review_document(change_set_id);
    persist_review_document(&review_path, &initial_review)
        .expect("initial review document should persist");

    let mut updated_review = initial_review.clone();
    updated_review.revision = 1;
    persist_review_document(&review_path, &updated_review)
        .expect("updated review document should persist");

    let loaded_review =
        load_review_document(&review_path, change_set_id).expect("review document should load");
    assert_eq!(loaded_review, updated_review);

    let review_dir = review_path
        .parent()
        .expect("review file should have a parent");
    let mut entry_names = fs::read_dir(review_dir)
        .expect("review directory should list")
        .map(|entry| {
            entry
                .expect("review directory entry should read")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    entry_names.sort();
    assert_eq!(entry_names, vec!["change-set-atomic-write.json".to_owned()]);

    let _ = fs::remove_dir_all(&review_root);
}

// Tests that the Windows replace helper overwrites an existing target in place.
#[cfg(windows)]
#[test]
fn replace_review_document_file_replaces_existing_target_on_windows() {
    let review_root =
        std::env::temp_dir().join(format!("termal-review-windows-replace-{}", Uuid::new_v4()));
    fs::create_dir_all(&review_root).expect("review root should exist");
    let review_path = review_root.join("review.json");
    let temp_path = review_root.join("review.tmp");

    fs::write(&review_path, b"original review").expect("existing review file should be written");
    fs::write(&temp_path, b"updated review").expect("temp review file should be written");

    replace_review_document_file(&temp_path, &review_path)
        .expect("existing review file should be replaced");

    assert_eq!(
        fs::read(&review_path).expect("replaced review file should read"),
        b"updated review"
    );
    assert!(
        !temp_path.exists(),
        "replacement temp file should be moved away"
    );

    let _ = fs::remove_dir_all(&review_root);
}

// Tests that a directory-sync failure after replacement does not surface as a write failure.
#[test]
fn persist_review_document_succeeds_when_directory_sync_fails_after_replace() {
    let review_root = std::env::temp_dir().join(format!(
        "termal-review-directory-sync-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&review_root).expect("review root should exist");
    let change_set_id = "change-set-directory-sync";
    let review_path = resolve_review_document_path(&review_root, change_set_id)
        .expect("review path should resolve");
    let initial_review = default_review_document(change_set_id);
    persist_review_document_with_directory_sync(&review_path, &initial_review, |_| Ok(()))
        .expect("initial review document should persist");

    let mut updated_review = initial_review.clone();
    updated_review.revision = 1;
    let result = persist_review_document_with_directory_sync(&review_path, &updated_review, |_| {
        Err(ApiError::internal("simulated directory sync failure"))
    });
    assert!(
        result.is_ok(),
        "post-rename directory sync failures should not fail the write"
    );

    let loaded_review =
        load_review_document(&review_path, change_set_id).expect("review document should load");
    assert_eq!(loaded_review, updated_review);

    let review_dir = review_path
        .parent()
        .expect("review file should have a parent");
    let mut entry_names = fs::read_dir(review_dir)
        .expect("review directory should list")
        .map(|entry| {
            entry
                .expect("review directory entry should read")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    entry_names.sort();
    assert_eq!(
        entry_names,
        vec!["change-set-directory-sync.json".to_owned()]
    );

    let _ = fs::remove_dir_all(&review_root);
}

// Tests that review change-set IDs reject empty values.
#[test]
fn resolve_review_document_path_rejects_empty_change_set_ids() {
    let review_root =
        std::env::temp_dir().join(format!("termal-review-empty-id-{}", Uuid::new_v4()));
    fs::create_dir_all(&review_root).expect("review root should exist");

    let error = match resolve_review_document_path(&review_root, "") {
        Ok(_) => panic!("empty change-set IDs should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, "changeSetId cannot be empty");

    let _ = fs::remove_dir_all(&review_root);
}

// Tests that review change-set IDs reject surrounding whitespace.
#[test]
fn resolve_review_document_path_rejects_change_set_ids_with_surrounding_whitespace() {
    let review_root =
        std::env::temp_dir().join(format!("termal-review-whitespace-id-{}", Uuid::new_v4()));
    fs::create_dir_all(&review_root).expect("review root should exist");

    let error = match resolve_review_document_path(&review_root, " change-set-whitespace ") {
        Ok(_) => panic!("surrounding whitespace should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error.message,
        "changeSetId may not have leading or trailing whitespace"
    );

    let _ = fs::remove_dir_all(&review_root);
}

// Tests that review change-set IDs reject overly long values.
#[test]
fn resolve_review_document_path_rejects_overlong_change_set_ids() {
    let review_root =
        std::env::temp_dir().join(format!("termal-review-long-id-{}", Uuid::new_v4()));
    fs::create_dir_all(&review_root).expect("review root should exist");
    let too_long_change_set_id = "a".repeat(MAX_REVIEW_CHANGE_SET_ID_LEN + 1);

    let error = match resolve_review_document_path(&review_root, &too_long_change_set_id) {
        Ok(_) => panic!("overlong change-set IDs should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error.message,
        format!("changeSetId is too long (max {MAX_REVIEW_CHANGE_SET_ID_LEN} bytes)")
    );

    let _ = fs::remove_dir_all(&review_root);
}

// Tests that review handlers validate change-set IDs before remote proxying.
#[tokio::test]
async fn review_handlers_validate_change_set_ids_before_remote_proxying() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let error = get_review(
        AxumPath(" change-set-remote ".to_owned()),
        Query(ReviewQuery {
            project_id: Some(project_id),
            session_id: None,
        }),
        State(state),
    )
    .await
    .expect_err("invalid remote review change-set ID should be rejected before proxying");

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error.message,
        "changeSetId may not have leading or trailing whitespace"
    );
}
// Tests that review change-set IDs reject invalid characters.
#[test]
fn resolve_review_document_path_rejects_change_set_ids_with_invalid_characters() {
    let review_root =
        std::env::temp_dir().join(format!("termal-review-invalid-id-{}", Uuid::new_v4()));
    fs::create_dir_all(&review_root).expect("review root should exist");

    let error = match resolve_review_document_path(&review_root, "change/set-invalid") {
        Ok(_) => panic!("invalid-character change-set IDs should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error.message,
        "changeSetId may only contain letters, numbers, '.', '-', and '_'"
    );

    let _ = fs::remove_dir_all(&review_root);
}

// Tests that review change-set IDs reject pure-dot values.
#[test]
fn resolve_review_document_path_rejects_change_set_ids_consisting_entirely_of_dots() {
    let review_root = std::env::temp_dir().join(format!("termal-review-dot-id-{}", Uuid::new_v4()));
    fs::create_dir_all(&review_root).expect("review root should exist");

    let error = match resolve_review_document_path(&review_root, "..") {
        Ok(_) => panic!("pure-dot change-set IDs should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error.message,
        "changeSetId must not consist entirely of dots"
    );

    let _ = fs::remove_dir_all(&review_root);
}

// Tests that review read routes wait for the review document lock.
#[tokio::test]
async fn review_read_routes_wait_for_review_document_lock() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-review-lock-test-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("review lock project root should exist");
    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Review Lock Project".to_owned()),
            root_path: project_root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .expect("review lock project should be created");
    let project_id = project.project_id;
    let app = app_router(state.clone());
    let change_set_id = "change-set-locked-read";
    let review_guard = state
        .review_documents_lock
        .lock()
        .expect("review documents mutex poisoned");
    let review_app = app.clone();
    let review_future = request_json::<ReviewDocumentResponse>(
        &review_app,
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/reviews/{change_set_id}?projectId={project_id}"
            ))
            .body(Body::empty())
            .unwrap(),
    );
    tokio::pin!(review_future);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut review_future)
            .await
            .is_err()
    );
    let summary_app = app.clone();
    let summary_future = request_json::<ReviewSummaryResponse>(
        &summary_app,
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/reviews/{change_set_id}/summary?projectId={project_id}"
            ))
            .body(Body::empty())
            .unwrap(),
    );
    tokio::pin!(summary_future);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut summary_future)
            .await
            .is_err()
    );
    drop(review_guard);
    let (review_status, review_response) = review_future.await;
    assert_eq!(review_status, StatusCode::OK);
    assert_eq!(review_response.review.change_set_id, change_set_id);
    assert_eq!(review_response.review.revision, 0);
    assert!(review_response.review.files.is_empty());
    assert!(review_response.review.threads.is_empty());
    assert!(
        review_response
            .review_file_path
            .ends_with("change-set-locked-read.json")
    );
    let (summary_status, summary_response) = summary_future.await;
    assert_eq!(summary_status, StatusCode::OK);
    assert_eq!(summary_response.change_set_id, change_set_id);
    assert_eq!(summary_response.thread_count, 0);
    assert_eq!(summary_response.open_thread_count, 0);
    assert_eq!(summary_response.resolved_thread_count, 0);
    assert_eq!(summary_response.comment_count, 0);
    assert!(!summary_response.has_threads);
    assert!(
        summary_response
            .review_file_path
            .ends_with("change-set-locked-read.json")
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(&project_root);
}

// Tests that update session settings route updates session name.
#[tokio::test]
async fn update_session_settings_route_updates_session_name() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "name": "Route Updated Session"
    }))
    .expect("settings route body should serialize");
    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/settings"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.name, "Route Updated Session");
    let _ = fs::remove_file(state.persistence_path.as_path());
}
// Tests that send message route accepts and queues prompt for busy session.
#[tokio::test]
async fn send_message_route_accepts_and_queues_prompt_for_busy_session() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
    }
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "text": "Queued route prompt",
        "expandedText": "Expanded queued route prompt"
    }))
    .expect("message route body should serialize");
    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/messages"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("queued session should be present");
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(session.pending_prompts.len(), 1);
    assert_eq!(session.pending_prompts[0].text, "Queued route prompt");
    assert_eq!(
        session.pending_prompts[0].expanded_text.as_deref(),
        Some("Expanded queued route prompt")
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}
// Tests that submit approval route updates Claude session and delivers runtime response.
#[tokio::test]
async fn submit_approval_route_updates_claude_session_and_delivers_runtime_response() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, input_rx) = test_claude_runtime_handle("claude-approval-route");
    let message_id = "approval-route-1".to_owned();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }
    state
        .push_message(
            &session_id,
            Message::Approval {
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Claude needs approval".to_owned(),
                command: "Edit src/main.rs".to_owned(),
                command_language: None,
                detail: "Need to update the route tests.".to_owned(),
                decision: ApprovalDecision::Pending,
            },
        )
        .expect("approval message should be recorded");
    state
        .register_claude_pending_approval(
            &session_id,
            message_id.clone(),
            ClaudePendingApproval {
                permission_mode_for_session: Some("acceptEdits".to_owned()),
                request_id: "claude-route-request".to_owned(),
                tool_input: json!({
                    "path": "src/main.rs"
                }),
            },
        )
        .expect("pending Claude approval should be registered");
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "decision": "acceptedForSession"
    }))
    .expect("approval route body should serialize");
    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/approvals/{message_id}"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated Claude session should be present");
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(
        session.preview,
        approval_preview_text("Claude", ApprovalDecision::AcceptedForSession)
    );
    assert!(session.messages.iter().any(|message| matches!(
        message,
        Message::Approval { id, decision, .. }
            if id == &message_id && *decision == ApprovalDecision::AcceptedForSession
    )));
    match input_rx.recv_timeout(Duration::from_millis(50)) {
        Ok(ClaudeRuntimeCommand::SetPermissionMode(mode)) => {
            assert_eq!(mode, "acceptEdits");
        }
        Ok(_) => panic!("expected Claude permission-mode update"),
        Err(err) => panic!("Claude permission-mode update should arrive: {err}"),
    }
    match input_rx.recv_timeout(Duration::from_millis(50)) {
        Ok(ClaudeRuntimeCommand::PermissionResponse(ClaudePermissionDecision::Allow {
            request_id,
            updated_input,
        })) => {
            assert_eq!(request_id, "claude-route-request");
            assert_eq!(updated_input, json!({ "path": "src/main.rs" }));
        }
        Ok(_) => panic!("expected Claude permission response"),
        Err(err) => panic!("Claude permission response should arrive: {err}"),
    }
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert!(record.pending_claude_approvals.is_empty());
    drop(inner);
    let _ = fs::remove_file(state.persistence_path.as_path());
}
// Tests that the empty SSE fallback payload carries an explicit fallback marker.
#[test]
fn empty_state_events_payload_carries_explicit_fallback_marker() {
    let payload: Value = serde_json::from_str(EMPTY_STATE_EVENTS_PAYLOAD.as_str())
        .expect("SSE fallback payload should parse");
    assert_eq!(payload["_sseFallback"], true);
    assert_eq!(payload["revision"], 0);
    assert!(payload.get("preferences").is_some());
    assert!(payload.get("sessions").is_some());

    let decoded: StateEventPayload = serde_json::from_str(EMPTY_STATE_EVENTS_PAYLOAD.as_str())
        .expect("fallback payload should decode as a state event payload");
    assert!(decoded.sse_fallback);
    assert_eq!(decoded.state.revision, 0);
}

// Tests that fallback SSE payloads can carry the recovered revision.
#[test]
fn fallback_state_events_payload_uses_supplied_revision() {
    let decoded: StateEventPayload = serde_json::from_str(
        &fallback_state_events_payload(42).expect("fallback payload should encode"),
    )
    .expect("fallback payload should decode as a state event payload");
    assert!(decoded.sse_fallback);
    assert_eq!(decoded.state.revision, 42);
}

// Tests that state events route streams initial state and live deltas.
#[tokio::test]
async fn state_events_route_streams_initial_state_and_live_deltas() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());
    let response = request_response(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/events")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .expect("SSE route should set a content type");
    assert!(content_type.starts_with("text/event-stream"));
    let mut body = Box::pin(response.into_body().into_data_stream());
    let initial_event = next_sse_event(&mut body).await;
    let (initial_name, initial_data) = parse_sse_event(&initial_event);
    assert_eq!(initial_name, "state");
    let initial_state: StateResponse =
        serde_json::from_str(&initial_data).expect("initial SSE payload should parse");
    assert!(
        initial_state
            .sessions
            .iter()
            .any(|session| session.id == session_id)
    );
    let message_id = state.allocate_message_id();
    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Live delta".to_owned(),
                expanded_text: None,
            },
        )
        .expect("delta message should be recorded");
    let delta_event = next_sse_event(&mut body).await;
    let (delta_name, delta_data) = parse_sse_event(&delta_event);
    assert_eq!(delta_name, "delta");
    let delta: Value = serde_json::from_str(&delta_data).expect("delta SSE payload should parse");
    assert_eq!(delta["type"], "messageCreated");
    assert_eq!(delta["sessionId"], session_id);
    assert_eq!(delta["messageId"], message_id);
    assert_eq!(delta["message"]["type"], "text");
    assert_eq!(delta["message"]["text"], "Live delta");
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that workspace layout mutations publish live SSE state updates with saved summaries.
#[tokio::test]
async fn state_events_route_streams_workspace_layout_summary_updates() {
    let state = test_app_state();
    let app = app_router(state.clone());
    let response = request_response(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/events")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = Box::pin(response.into_body().into_data_stream());

    let initial_event = next_sse_event(&mut body).await;
    let (initial_name, initial_data) = parse_sse_event(&initial_event);
    assert_eq!(initial_name, "state");
    let initial_state: StateResponse =
        serde_json::from_str(&initial_data).expect("initial SSE payload should parse");
    assert!(initial_state.workspaces.is_empty());

    let create_layout_body = serde_json::to_vec(&json!({
        "controlPanelSide": "left",
        "workspace": { "panes": [] }
    }))
    .expect("workspace layout body should serialize");
    let (save_status, _save_response): (StatusCode, WorkspaceLayoutResponse) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/workspaces/workspace-live")
            .header("content-type", "application/json")
            .body(Body::from(create_layout_body))
            .unwrap(),
    )
    .await;
    assert_eq!(save_status, StatusCode::OK);

    let saved_event = next_sse_event(&mut body).await;
    let (saved_name, saved_data) = parse_sse_event(&saved_event);
    assert_eq!(saved_name, "state");
    let saved_state: StateResponse =
        serde_json::from_str(&saved_data).expect("saved SSE payload should parse");
    assert_eq!(saved_state.workspaces.len(), 1);
    assert_eq!(saved_state.workspaces[0].id, "workspace-live");
    assert_eq!(saved_state.workspaces[0].revision, 1);
    assert_eq!(
        saved_state.workspaces[0].control_panel_side,
        WorkspaceControlPanelSide::Left
    );

    let update_layout_body = serde_json::to_vec(&json!({
        "controlPanelSide": "right",
        "workspace": {
            "panes": [
                {
                    "id": "pane-1",
                    "tabs": []
                }
            ]
        }
    }))
    .expect("updated workspace layout body should serialize");
    let (update_status, _update_response): (StatusCode, WorkspaceLayoutResponse) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/workspaces/workspace-live")
            .header("content-type", "application/json")
            .body(Body::from(update_layout_body))
            .unwrap(),
    )
    .await;
    assert_eq!(update_status, StatusCode::OK);

    let updated_event = next_sse_event(&mut body).await;
    let (updated_name, updated_data) = parse_sse_event(&updated_event);
    assert_eq!(updated_name, "state");
    let updated_state: StateResponse =
        serde_json::from_str(&updated_data).expect("updated SSE payload should parse");
    assert_eq!(updated_state.workspaces.len(), 1);
    assert_eq!(updated_state.workspaces[0].id, "workspace-live");
    assert_eq!(updated_state.workspaces[0].revision, 2);
    assert_eq!(
        updated_state.workspaces[0].control_panel_side,
        WorkspaceControlPanelSide::Right
    );

    let (delete_status, _delete_response): (StatusCode, WorkspaceLayoutsResponse) = request_json(
        &app,
        Request::builder()
            .method("DELETE")
            .uri("/api/workspaces/workspace-live")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(delete_status, StatusCode::OK);

    let deleted_event = next_sse_event(&mut body).await;
    let (deleted_name, deleted_data) = parse_sse_event(&deleted_event);
    assert_eq!(deleted_name, "state");
    let deleted_state: StateResponse =
        serde_json::from_str(&deleted_data).expect("deleted SSE payload should parse");
    assert!(deleted_state.workspaces.is_empty());
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that state events route streams orchestrator creation state and live orchestrator deltas.
#[tokio::test]
async fn state_events_route_streams_orchestrator_creation_state_and_live_orchestrator_deltas() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-events-route-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("events project root should exist");
    let project_id = create_test_project(&state, &project_root, "Events Orchestrator Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let app = app_router(state.clone());
    let response = request_response(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/events")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = Box::pin(response.into_body().into_data_stream());

    let initial_event = next_sse_event(&mut body).await;
    let (initial_name, initial_data) = parse_sse_event(&initial_event);
    assert_eq!(initial_name, "state");
    let initial_state: StateResponse =
        serde_json::from_str(&initial_data).expect("initial SSE payload should parse");
    assert!(initial_state.orchestrators.is_empty());

    let created = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created");
    let instance_id = created.orchestrator.id.clone();
    let created_session_ids = created
        .orchestrator
        .session_instances
        .iter()
        .map(|instance| instance.session_id.clone())
        .collect::<Vec<_>>();

    let created_event = next_sse_event(&mut body).await;
    let (created_name, created_data) = parse_sse_event(&created_event);
    assert_eq!(created_name, "state");
    let created_state: StateResponse =
        serde_json::from_str(&created_data).expect("create SSE payload should parse");
    let created_orchestrator = created_state
        .orchestrators
        .iter()
        .find(|instance| instance.id == instance_id)
        .expect("create SSE state should include the orchestrator instance");
    assert_eq!(
        created_orchestrator.status,
        OrchestratorInstanceStatus::Running
    );
    for session_id in &created_session_ids {
        assert!(
            created_state
                .sessions
                .iter()
                .any(|session| session.id == *session_id),
            "create SSE state should include orchestrator session {session_id}"
        );
    }

    state
        .pause_orchestrator_instance(&instance_id)
        .expect("pause route should update orchestrator state");

    let delta_event = next_sse_event(&mut body).await;
    let (delta_name, delta_data) = parse_sse_event(&delta_event);
    assert_eq!(delta_name, "delta");
    let delta: Value = serde_json::from_str(&delta_data).expect("delta SSE payload should parse");
    assert_eq!(delta["type"], "orchestratorsUpdated");
    assert!(
        delta["orchestrators"]
            .as_array()
            .is_some_and(|instances| instances.iter().any(|instance| {
                instance["id"] == Value::String(instance_id.clone())
                    && instance["status"] == Value::String("paused".to_owned())
            }))
    );
    let delta_session_ids = delta["sessions"]
        .as_array()
        .expect("orchestrator delta should include referenced sessions")
        .iter()
        .map(|session| {
            session["id"]
                .as_str()
                .expect("delta session should include an ID")
                .to_owned()
        })
        .collect::<HashSet<_>>();
    assert_eq!(
        delta_session_ids,
        created_session_ids.into_iter().collect::<HashSet<_>>()
    );

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that Codex thread action routes update session state.
#[tokio::test]
async fn codex_thread_action_routes_update_session_state() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "thread-live".to_owned())
        .unwrap();
    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "stale local message".to_owned(),
                expanded_text: None,
            },
        )
        .unwrap();
    let (runtime, input_rx, _process) = test_shared_codex_runtime("shared-codex-route-actions");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    std::thread::spawn(move || {
        for expected_method in ["thread/archive", "thread/unarchive", "thread/rollback"] {
            let command = input_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("Codex thread action should arrive");
            match command {
                CodexRuntimeCommand::JsonRpcRequest {
                    method,
                    params,
                    response_tx,
                    ..
                } => {
                    assert_eq!(method, expected_method);
                    assert_eq!(params["threadId"], "thread-live");
                    if method == "thread/rollback" {
                        assert_eq!(params["numTurns"], 2);
                        let _ = response_tx.send(Ok(json!({
                            "thread": {
                                "preview": "Rolled back preview",
                                "turns": [
                                    {
                                        "id": "turn-rollback",
                                        "status": "completed",
                                        "items": [
                                            {
                                                "id": "rollback-user",
                                                "type": "userMessage",
                                                "content": [
                                                    {
                                                        "type": "text",
                                                        "text": "Current diff state"
                                                    }
                                                ]
                                            },
                                            {
                                                "id": "rollback-agent",
                                                "type": "agentMessage",
                                                "text": "Rollback synced."
                                            }
                                        ]
                                    }
                                ]
                            }
                        })));
                        continue;
                    }
                    let _ = response_tx.send(Ok(json!({})));
                }
                _ => panic!("expected shared Codex JSON-RPC request"),
            }
        }
    });

    let app = app_router(state);
    let (archive_status, archive_response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/codex/thread/archive"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(archive_status, StatusCode::OK);
    let archived_session = archive_response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(
        archived_session.codex_thread_state,
        Some(CodexThreadState::Archived)
    );

    let (unarchive_status, unarchive_response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/codex/thread/unarchive"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(unarchive_status, StatusCode::OK);
    let restored_session = unarchive_response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(
        restored_session.codex_thread_state,
        Some(CodexThreadState::Active)
    );

    let (rollback_status, rollback_response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/codex/thread/rollback"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"numTurns":2}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(rollback_status, StatusCode::OK);
    let rollback_session = rollback_response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(matches!(
        rollback_session.messages.first(),
        Some(Message::Text { author: Author::You, text, .. }) if text == "Current diff state"
    ));
    assert!(matches!(
        rollback_session.messages.get(1),
        Some(Message::Text { author: Author::Assistant, text, .. }) if text == "Rollback synced."
    ));
    assert!(!rollback_session.messages.iter().any(
        |message| matches!(message, Message::Markdown { title, .. }
            if title == "Archived Codex thread"
                || title == "Restored Codex thread"
                || title == "Rolled back Codex thread")
    ));
    assert!(!rollback_session.messages.iter().any(
        |message| matches!(message, Message::Text { text, .. } if text == "stale local message")
    ));
}

// Tests that Codex thread rollback route falls back when history is unavailable.
#[tokio::test]
async fn codex_thread_rollback_route_falls_back_when_history_is_unavailable() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "thread-live".to_owned())
        .unwrap();
    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "local history".to_owned(),
                expanded_text: None,
            },
        )
        .unwrap();
    let (runtime, input_rx, _process) =
        test_shared_codex_runtime("shared-codex-route-rollback-fallback");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Codex rollback command should arrive");
        match command {
            CodexRuntimeCommand::JsonRpcRequest {
                method,
                params,
                response_tx,
                ..
            } => {
                assert_eq!(method, "thread/rollback");
                assert_eq!(params["threadId"], "thread-live");
                assert_eq!(params["numTurns"], 1);
                let _ = response_tx.send(Ok(json!({
                    "thread": {
                        "preview": "Fallback preview"
                    }
                })));
            }
            _ => panic!("expected shared Codex JSON-RPC request"),
        }
    });

    let app = app_router(state);
    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/codex/thread/rollback"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"numTurns":1}"#))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "local history"
    ));
    assert!(matches!(
        session.messages.last(),
        Some(Message::Markdown { title, markdown, .. })
            if title == "Rolled back Codex thread"
                && markdown.contains("Codex did not return the updated thread history")
    ));
}

// Tests that Codex thread fork route returns created response.
#[tokio::test]
async fn codex_thread_fork_route_returns_created_response() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Route Review".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5.4".to_owned()),
            approval_policy: Some(CodexApprovalPolicy::Never),
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    state
        .set_external_session_id(&created.session_id, "thread-origin".to_owned())
        .unwrap();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("source Codex session should exist");
        inner.sessions[index].session.model_options =
            vec![SessionModelOption::plain("gpt-5.4", "gpt-5.4")];
    }

    let (runtime, input_rx, _process) = test_shared_codex_runtime("shared-codex-route-fork");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Codex fork command should arrive");
        match command {
            CodexRuntimeCommand::JsonRpcRequest {
                method,
                params,
                response_tx,
                ..
            } => {
                assert_eq!(method, "thread/fork");
                assert_eq!(params["threadId"], "thread-origin");
                let _ = response_tx.send(Ok(json!({
                    "thread": {
                        "id": "thread-forked",
                        "name": "Forked Review",
                        "preview": "Forked preview",
                        "turns": [
                            {
                                "id": "turn-forked",
                                "status": "completed",
                                "items": [
                                    {
                                        "id": "fork-user",
                                        "type": "userMessage",
                                        "content": [
                                            {
                                                "type": "text",
                                                "text": "Fork context"
                                            }
                                        ]
                                    },
                                    {
                                        "id": "fork-agent",
                                        "type": "agentMessage",
                                        "text": "Ready to continue."
                                    }
                                ]
                            }
                        ]
                    },
                    "model": "gpt-5.5",
                    "approvalPolicy": "on-request",
                    "sandbox": {
                        "type": "workspaceWrite"
                    },
                    "reasoningEffort": "high",
                    "cwd": "/tmp/forked",
                })));
            }
            _ => panic!("expected shared Codex JSON-RPC request"),
        }
    });

    let app = app_router(state);
    let (status, response): (StatusCode, CreateSessionResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{}/codex/thread/fork",
                created.session_id
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    let forked_session = response
        .state
        .sessions
        .iter()
        .find(|session| session.id == response.session_id)
        .expect("forked session should be present");
    assert_eq!(
        forked_session.codex_thread_state,
        Some(CodexThreadState::Active)
    );
    assert_eq!(
        forked_session.external_session_id.as_deref(),
        Some("thread-forked")
    );
    assert!(matches!(
        forked_session.messages.first(),
        Some(Message::Text { author: Author::You, text, .. }) if text == "Fork context"
    ));
    assert!(matches!(
        forked_session.messages.get(1),
        Some(Message::Text { author: Author::Assistant, text, .. }) if text == "Ready to continue."
    ));
}

// Tests that shared Codex task complete event buffers subagent result until final agent message.
#[test]
fn shared_codex_task_complete_event_buffers_subagent_result_until_final_agent_message() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-task-complete");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-sub-1"
            }
        }
    });
    let task_complete = json!({
        "method": "codex/event/task_complete",
        "params": {
            "conversationId": "conversation-123",
            "msg": {
                "last_agent_message": "Reviewer found a real bug.",
                "turn_id": "turn-sub-1",
                "type": "task_complete"
            }
        }
    });
    let final_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-sub-1",
            "msg": {
                "message": "Final shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &task_complete,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &final_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.preview, "Final shared Codex answer.");
    assert_eq!(session.messages.len(), 2);
    assert!(matches!(
        session.messages.first(),
        Some(Message::SubagentResult {
            title,
            summary,
            conversation_id,
            turn_id,
            ..
        }) if title == "Subagent completed"
            && summary == "Reviewer found a real bug."
            && conversation_id.as_deref() == Some("conversation-123")
            && turn_id.as_deref() == Some("turn-sub-1")
    ));
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Final shared Codex answer."
    ));
}

// Tests that shared Codex agent message event without turn ID uses active turn.
#[test]
fn shared_codex_agent_message_event_without_turn_id_uses_active_turn() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-agent-final-no-turn-id");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-no-id"
            }
        }
    });
    let final_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "msg": {
                "message": "Final shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &final_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.preview, "Final shared Codex answer.");
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Final shared Codex answer."
    ));
}

// Tests that shared Codex agent message event ignores stale turn ID from params ID.
#[test]
fn shared_codex_agent_message_event_ignores_stale_turn_id_from_params_id() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-agent-final-stale-params-id");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started_previous = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stale"
            }
        }
    });
    let turn_started_current = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-current"
            }
        }
    });
    let stale_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-stale",
            "msg": {
                "message": "Stale shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });
    let current_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-current",
            "msg": {
                "message": "Current shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started_previous,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_started_current,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &stale_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &current_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.preview, "Current shared Codex answer.");
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Current shared Codex answer."
    ));
}

// Tests that shared Codex task complete event stays in current turn after prior assistant message.
#[test]
fn shared_codex_task_complete_event_stays_in_current_turn_after_prior_assistant_message() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: "assistant-previous".to_owned(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Previous shared Codex answer.".to_owned(),
                expanded_text: None,
            },
        )
        .unwrap();
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-task-complete-order");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-sub-2"
            }
        }
    });
    let task_complete = json!({
        "method": "codex/event/task_complete",
        "params": {
            "conversationId": "conversation-123",
            "msg": {
                "last_agent_message": "Reviewer found a real bug.",
                "turn_id": "turn-sub-2",
                "type": "task_complete"
            }
        }
    });
    let final_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-sub-2",
            "msg": {
                "message": "Current shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &task_complete,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(matches!(
        session.messages.as_slice(),
        [Message::Text { text, .. }] if text == "Previous shared Codex answer."
    ));

    handle_shared_codex_app_server_message(
        &final_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.messages.len(), 3);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Previous shared Codex answer."
    ));
    assert!(matches!(
        session.messages.get(1),
        Some(Message::SubagentResult {
            title,
            summary,
            conversation_id,
            turn_id,
            ..
        }) if title == "Subagent completed"
            && summary == "Reviewer found a real bug."
            && conversation_id.as_deref() == Some("conversation-123")
            && turn_id.as_deref() == Some("turn-sub-2")
    ));
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Current shared Codex answer."
    ));
}

// Tests that shared Codex task complete event without active turn is ignored.
#[test]
fn shared_codex_task_complete_event_without_active_turn_is_ignored() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-task-complete-no-active-turn");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Idle;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let task_complete = json!({
        "method": "codex/event/task_complete",
        "params": {
            "conversationId": "conversation-123",
            "msg": {
                "last_agent_message": "Stale hidden summary.",
                "type": "task_complete"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &task_complete,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert!(session.messages.is_empty());
}

// Tests that shared Codex task complete event after streaming output inserts before answer.
#[test]
fn shared_codex_task_complete_event_after_streaming_output_inserts_before_answer() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-task-complete-late");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-sub-3"
            }
        }
    });
    let delta_message = json!({
        "method": "codex/event/agent_message_content_delta",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-sub-3",
            "msg": {
                "delta": "Current shared Codex answer.",
                "item_id": "msg-123",
                "thread_id": "conversation-123",
                "turn_id": "turn-sub-3",
                "type": "agent_message_content_delta"
            }
        }
    });
    let task_complete = json!({
        "method": "codex/event/task_complete",
        "params": {
            "conversationId": "conversation-123",
            "msg": {
                "last_agent_message": "Reviewer found a real bug.",
                "turn_id": "turn-sub-3",
                "type": "task_complete"
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-sub-3",
                "error": null
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &delta_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &task_complete,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_completed,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.preview, "Current shared Codex answer.");
    assert_eq!(session.messages.len(), 2);
    assert!(matches!(
        session.messages.first(),
        Some(Message::SubagentResult {
            title,
            summary,
            conversation_id,
            turn_id,
            ..
        }) if title == "Subagent completed"
            && summary == "Reviewer found a real bug."
            && conversation_id.as_deref() == Some("conversation-123")
            && turn_id.as_deref() == Some("turn-sub-3")
    ));
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Current shared Codex answer."
    ));
}

// Tests that shared Codex task complete event ignores stale summary from previous turn.
#[test]
fn shared_codex_task_complete_event_ignores_stale_summary_from_previous_turn() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-task-complete-stale");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started_previous = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stale"
            }
        }
    });
    let turn_started_current = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-current"
            }
        }
    });
    let stale_task_complete = json!({
        "method": "codex/event/task_complete",
        "params": {
            "conversationId": "conversation-123",
            "msg": {
                "last_agent_message": "Stale hidden summary.",
                "turn_id": "turn-stale",
                "type": "task_complete"
            }
        }
    });
    let final_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-current",
            "msg": {
                "message": "Current shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started_previous,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_started_current,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &stale_task_complete,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &final_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.preview, "Current shared Codex answer.");
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Current shared Codex answer."
    ));
}

// Tests that shared Codex task complete event drops buffered summary on failed turn.
#[test]
fn shared_codex_task_complete_event_drops_buffered_summary_on_failed_turn() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-task-complete-error");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-sub-4"
            }
        }
    });
    let task_complete = json!({
        "method": "codex/event/task_complete",
        "params": {
            "conversationId": "conversation-123",
            "msg": {
                "last_agent_message": "Reviewer found a real bug.",
                "turn_id": "turn-sub-4",
                "type": "task_complete"
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-sub-4",
                "error": {
                    "message": "stream failed"
                }
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &task_complete,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_completed,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.status, SessionStatus::Error);
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Turn failed: stream failed"
    ));
}

// Tests that shared Codex turn completed flushes buffered subagent results after output started.
#[test]
fn shared_codex_turn_completed_flushes_buffered_subagent_results_after_output_started() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-turn-completed-flushes-buffer");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                turn_id: Some("turn-sub-5".to_owned()),
                turn_state: CodexTurnState {
                    assistant_output_started: true,
                    pending_subagent_results: vec![PendingSubagentResult {
                        title: "Subagent completed".to_owned(),
                        summary: "Buffered reviewer summary.".to_owned(),
                        conversation_id: Some("conversation-123".to_owned()),
                        turn_id: Some("turn-sub-5".to_owned()),
                    }],
                    ..CodexTurnState::default()
                },
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-sub-5",
                "error": null
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_completed,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::SubagentResult {
            title,
            summary,
            conversation_id,
            turn_id,
            ..
        }) if title == "Subagent completed"
            && summary == "Buffered reviewer summary."
            && conversation_id.as_deref() == Some("conversation-123")
            && turn_id.as_deref() == Some("turn-sub-5")
    ));
}

// Tests that shared Codex item completed event records agent message.
#[test]
fn shared_codex_item_completed_event_records_agent_message() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-item-completed");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-123"
            }
        }
    });
    let message = json!({
        "method": "codex/event/item_completed",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "item": {
                    "content": [
                        {
                            "text": "Hello.",
                            "type": "Text"
                        }
                    ],
                    "id": "msg-123",
                    "phase": "final_answer",
                    "type": "AgentMessage"
                },
                "thread_id": "conversation-123",
                "turn_id": "turn-123",
                "type": "item_completed"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Hello."
    ));
}

// Tests that shared Codex item completed event ignores stale turn ID from params ID.
#[test]
fn shared_codex_item_completed_event_ignores_stale_turn_id_from_params_id() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-item-completed-stale-params-id");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started_previous = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stale"
            }
        }
    });
    let turn_started_current = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-current"
            }
        }
    });
    let stale_message = json!({
        "method": "codex/event/item_completed",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-stale",
            "msg": {
                "item": {
                    "content": [
                        {
                            "text": "Stale shared Codex answer.",
                            "type": "Text"
                        }
                    ],
                    "id": "msg-stale",
                    "phase": "final_answer",
                    "type": "AgentMessage"
                },
                "thread_id": "conversation-123",
                "type": "item_completed"
            }
        }
    });
    let current_message = json!({
        "method": "codex/event/item_completed",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-current",
            "msg": {
                "item": {
                    "content": [
                        {
                            "text": "Current shared Codex answer.",
                            "type": "Text"
                        }
                    ],
                    "id": "msg-current",
                    "phase": "final_answer",
                    "type": "AgentMessage"
                },
                "thread_id": "conversation-123",
                "type": "item_completed"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started_previous,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_started_current,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &stale_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &current_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.preview, "Current shared Codex answer.");
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Current shared Codex answer."
    ));
}

// Tests that shared Codex item completed event concatenates multipart agent message.
#[test]
fn shared_codex_item_completed_event_concatenates_multipart_agent_message() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-item-completed-multipart");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-123"
            }
        }
    });
    let message = json!({
        "method": "codex/event/item_completed",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "item": {
                    "content": [
                        {
                            "text": "Hello",
                            "type": "Text"
                        },
                        {
                            "metadata": {
                                "ignored": true
                            },
                            "type": "Reasoning"
                        },
                        {
                            "text": ", world.",
                            "type": "Text"
                        }
                    ],
                    "id": "msg-123",
                    "phase": "final_answer",
                    "type": "AgentMessage"
                },
                "thread_id": "conversation-123",
                "turn_id": "turn-123",
                "type": "item_completed"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Hello, world."
    ));
}

// Tests that shared Codex agent message content delta event ignores stale turn ID from params ID.
#[test]
fn shared_codex_agent_message_content_delta_event_ignores_stale_turn_id_from_params_id() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-agent-delta-stale-params-id");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started_previous = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stale"
            }
        }
    });
    let turn_started_current = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-current"
            }
        }
    });
    let stale_delta_message = json!({
        "method": "codex/event/agent_message_content_delta",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-stale",
            "msg": {
                "delta": "Stale shared Codex answer.",
                "item_id": "msg-stale",
                "thread_id": "conversation-123",
                "type": "agent_message_content_delta"
            }
        }
    });
    let current_delta_message = json!({
        "method": "codex/event/agent_message_content_delta",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-current",
            "msg": {
                "delta": "Current shared Codex answer.",
                "item_id": "msg-current",
                "thread_id": "conversation-123",
                "type": "agent_message_content_delta"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started_previous,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_started_current,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &stale_delta_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &current_delta_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.preview, "Current shared Codex answer.");
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Current shared Codex answer."
    ));
}

// Tests that shared Codex agent message final event appends missing suffix after streamed delta.
#[test]
fn shared_codex_agent_message_final_event_appends_missing_suffix_after_streamed_delta() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-agent-final-suffix");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-123"
            }
        }
    });
    let delta_message = json!({
        "method": "codex/event/agent_message_content_delta",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "delta": "Hello",
                "item_id": "msg-123",
                "thread_id": "conversation-123",
                "turn_id": "turn-123",
                "type": "agent_message_content_delta"
            }
        }
    });
    let final_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "message": "Hello there.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &delta_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &final_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Hello there."
    ));
}

// Tests that shared Codex agent message final event replaces divergent streamed text.
#[test]
fn shared_codex_agent_message_final_event_replaces_divergent_streamed_text() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-agent-final-replace");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-123"
            }
        }
    });
    let delta_message = json!({
        "method": "codex/event/agent_message_content_delta",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "delta": "Hello from stream",
                "item_id": "msg-123",
                "thread_id": "conversation-123",
                "turn_id": "turn-123",
                "type": "agent_message_content_delta"
            }
        }
    });
    let final_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "message": "Different final answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });
    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &delta_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Hello from stream"
    ));
    handle_shared_codex_app_server_message(
        &final_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.preview, "Different final answer.");
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Different final answer."
    ));
}
// Tests that shared Codex agent message content delta streams without duplicate final message.
#[test]
fn shared_codex_agent_message_content_delta_streams_without_duplicate_final_message() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-agent-delta");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-123"
            }
        }
    });
    let app_server_delta_message = json!({
        "method": "item/agentMessage/delta",
        "params": {
            "delta": "Hello.",
            "itemId": "msg-123"
        }
    });
    let delta_message = json!({
        "method": "codex/event/agent_message_content_delta",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "delta": "Hello.",
                "item_id": "msg-123",
                "thread_id": "conversation-123",
                "turn_id": "turn-123",
                "type": "agent_message_content_delta"
            }
        }
    });
    let final_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "message": "Hello.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &app_server_delta_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &delta_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &final_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Hello."
    ));
}

// Tests that shared Codex final agent messages still land after turn completion.
#[test]
fn shared_codex_agent_message_event_after_turn_completed_is_recorded() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-agent-final-after-turn-completed");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-finished"
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-finished",
                "error": null
            }
        }
    });
    let late_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-finished",
            "msg": {
                "message": "Late shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_completed,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &late_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Late shared Codex answer."
    ));
}

// Tests that a late message from the previous turn is rejected once the next turn starts.
#[test]
fn shared_codex_previous_turn_message_is_ignored_after_next_turn_starts() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-late-previous-turn-message");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let first_turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-first"
            }
        }
    });
    let first_turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-first",
                "error": null
            }
        }
    });
    let second_turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-second"
            }
        }
    });
    let late_first_turn_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-first",
            "msg": {
                "message": "Late first-turn answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    for message in [
        &first_turn_started,
        &first_turn_completed,
        &second_turn_started,
        &late_first_turn_message,
    ] {
        handle_shared_codex_app_server_message(
            message,
            &state,
            &runtime.runtime_id,
            &pending_requests,
            &runtime.sessions,
            &runtime.thread_sessions,
            &mpsc::channel::<CodexRuntimeCommand>().0,
        )
        .unwrap();
    }

    {
        let sessions = runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        let session_state = sessions
            .get(&session_id)
            .expect("shared Codex session state should exist");
        assert_eq!(session_state.turn_id.as_deref(), Some("turn-second"));
        assert!(session_state.completed_turn_id.is_none());
    }

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert!(session.messages.is_empty());
}

// Tests that shared Codex app-server agentMessage completion after turn completion is recorded.
#[test]
fn shared_codex_app_server_agent_message_completed_after_turn_completed_is_recorded() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-agent-item-after-turn-completed");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-finished"
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-finished",
                "error": null
            }
        }
    });
    let late_item = json!({
        "method": "item/completed",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "id": "msg-final",
                "type": "agentMessage",
                "text": "Late app-server final answer."
            }
        }
    });

    for message in [&turn_started, &turn_completed, &late_item] {
        handle_shared_codex_app_server_message(
            message,
            &state,
            &runtime.runtime_id,
            &pending_requests,
            &runtime.sessions,
            &runtime.thread_sessions,
            &mpsc::channel::<CodexRuntimeCommand>().0,
        )
        .unwrap();
    }

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Late app-server final answer."
    ));
}

// Tests that shared Codex app-server non-message item completion after turn completion is ignored.
#[test]
fn shared_codex_app_server_item_completed_after_turn_completed_is_ignored() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-app-server-item-completed-after-turn-completed");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-finished"
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-finished",
                "error": null
            }
        }
    });
    let late_item = json!({
        "method": "item/completed",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "type": "commandExecution",
                "command": "pwd",
                "aggregatedOutput": "C:/github/Personal/TermAl",
                "status": "completed",
                "exitCode": 0
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_completed,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &late_item,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert!(session.messages.is_empty());
}

// Tests that completed-turn cleanup bounds late-event acceptance and clears residual turn state.
#[test]
fn shared_codex_completed_turn_cleanup_expires_late_event_window() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-completed-turn-cleanup");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-finished"
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-finished",
                "error": null
            }
        }
    });
    let late_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-finished",
            "msg": {
                "message": "Late shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_completed,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    {
        let mut sessions = runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        let session_state = sessions
            .get_mut(&session_id)
            .expect("shared Codex session state should exist");
        assert_eq!(
            session_state.completed_turn_id.as_deref(),
            Some("turn-finished")
        );
        session_state.turn_state.current_agent_message_id = Some("msg-final".to_owned());
        session_state
            .turn_state
            .streamed_agent_message_text_by_item_id
            .insert("msg-final".to_owned(), "stale buffered text".to_owned());
        session_state
            .turn_state
            .streamed_agent_message_item_ids
            .insert("msg-final".to_owned());
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let cleanup_complete = {
            let sessions = runtime
                .sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            let session_state = sessions
                .get(&session_id)
                .expect("shared Codex session state should exist");
            session_state.completed_turn_id.is_none()
                && session_state.turn_state.current_agent_message_id.is_none()
                && session_state
                    .turn_state
                    .streamed_agent_message_text_by_item_id
                    .is_empty()
                && session_state
                    .turn_state
                    .streamed_agent_message_item_ids
                    .is_empty()
        };
        if cleanup_complete {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "completed turn cleanup should clear residual turn state"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    handle_shared_codex_app_server_message(
        &late_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());
}

// Tests that shared Codex turn start clears command recorder keys for a new prompt.
#[test]
fn shared_codex_turn_started_clears_command_recorder_keys_for_new_prompt() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-turn-start-clears-command-keys");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started_one = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-1"
            }
        }
    });
    let item_started_one = json!({
        "method": "item/started",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "type": "webSearch",
                "query": "rust anyhow",
                "action": {
                    "type": "search",
                    "queries": ["rust anyhow"]
                }
            }
        }
    });
    let item_completed_one = json!({
        "method": "item/completed",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "type": "webSearch",
                "query": "rust anyhow",
                "action": {
                    "type": "search",
                    "queries": ["rust anyhow"]
                }
            }
        }
    });
    let turn_completed_one = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-1",
                "error": null
            }
        }
    });
    let turn_started_two = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-2"
            }
        }
    });
    let item_started_two = json!({
        "method": "item/started",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "type": "webSearch",
                "query": "serde_json value",
                "action": {
                    "type": "search",
                    "queries": ["serde_json value"]
                }
            }
        }
    });
    let item_completed_two = json!({
        "method": "item/completed",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "type": "webSearch",
                "query": "serde_json value",
                "action": {
                    "type": "search",
                    "queries": ["serde_json value"]
                }
            }
        }
    });

    for message in [
        &turn_started_one,
        &item_started_one,
        &item_completed_one,
        &turn_completed_one,
        &turn_started_two,
        &item_started_two,
        &item_completed_two,
    ] {
        handle_shared_codex_app_server_message(
            message,
            &state,
            &runtime.runtime_id,
            &pending_requests,
            &runtime.sessions,
            &runtime.thread_sessions,
            &mpsc::channel::<CodexRuntimeCommand>().0,
        )
        .unwrap();
    }

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    let command_messages = session
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Command {
                command,
                output,
                status,
                ..
            } => Some((command.clone(), output.clone(), *status)),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        command_messages,
        vec![
            (
                "Web search: rust anyhow".to_owned(),
                "rust anyhow".to_owned(),
                CommandStatus::Success,
            ),
            (
                "Web search: serde_json value".to_owned(),
                "serde_json value".to_owned(),
                CommandStatus::Success,
            ),
        ]
    );
}

// Tests that shared Codex turn-completed errors clear recorder state before the next turn.
#[test]
fn shared_codex_turn_completed_error_clears_recorder_state() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-turn-completed-error-clears-recorder");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                pending_turn_start_request_id: Some("turn-start-1".to_owned()),
                recorder: SessionRecorderState {
                    command_messages: HashMap::from([(
                        "search".to_owned(),
                        "command-message".to_owned(),
                    )]),
                    parallel_agents_messages: HashMap::from([(
                        "parallel".to_owned(),
                        "parallel-message".to_owned(),
                    )]),
                    streaming_text_message_id: Some("stream-message".to_owned()),
                },
                thread_id: Some("conversation-123".to_owned()),
                turn_id: Some("turn-1".to_owned()),
                completed_turn_id: None,
                turn_started: true,
                turn_state: CodexTurnState {
                    current_agent_message_id: Some("stream-message".to_owned()),
                    assistant_output_started: true,
                    ..CodexTurnState::default()
                },
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-1",
                "error": {
                    "message": "Turn failed"
                }
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_completed,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .expect("turn/completed error should be handled");

    let sessions = runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    let session_state = sessions
        .get(&session_id)
        .expect("shared Codex session state should exist");
    assert!(session_state.recorder.command_messages.is_empty());
    assert!(session_state.recorder.parallel_agents_messages.is_empty());
    assert_eq!(session_state.recorder.streaming_text_message_id, None);
    assert_eq!(session_state.turn_id, None);
    assert!(!session_state.turn_started);
    assert_eq!(session_state.turn_state.current_agent_message_id, None);
    assert!(!session_state.turn_state.assistant_output_started);
    drop(sessions);

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.status, SessionStatus::Error);
}

// Tests that shared Codex error notifications clear recorder state before the next turn.
#[test]
fn shared_codex_error_notification_clears_recorder_state() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-error-notification-clears-recorder");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                pending_turn_start_request_id: Some("turn-start-1".to_owned()),
                recorder: SessionRecorderState {
                    command_messages: HashMap::from([(
                        "search".to_owned(),
                        "command-message".to_owned(),
                    )]),
                    parallel_agents_messages: HashMap::from([(
                        "parallel".to_owned(),
                        "parallel-message".to_owned(),
                    )]),
                    streaming_text_message_id: Some("stream-message".to_owned()),
                },
                thread_id: Some("conversation-123".to_owned()),
                turn_id: Some("turn-1".to_owned()),
                completed_turn_id: None,
                turn_started: true,
                turn_state: CodexTurnState {
                    current_agent_message_id: Some("stream-message".to_owned()),
                    assistant_output_started: true,
                    ..CodexTurnState::default()
                },
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let error_notice = json!({
        "method": "error",
        "params": {
            "threadId": "conversation-123",
            "turnId": "turn-1",
            "message": "Codex runtime failure"
        }
    });

    handle_shared_codex_app_server_message(
        &error_notice,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .expect("error notification should be handled");

    let sessions = runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    let session_state = sessions
        .get(&session_id)
        .expect("shared Codex session state should exist");
    assert!(session_state.recorder.command_messages.is_empty());
    assert!(session_state.recorder.parallel_agents_messages.is_empty());
    assert_eq!(session_state.recorder.streaming_text_message_id, None);
    assert_eq!(session_state.turn_id, None);
    assert!(!session_state.turn_started);
    assert_eq!(session_state.turn_state.current_agent_message_id, None);
    assert!(!session_state.turn_state.assistant_output_started);
    drop(sessions);

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.status, SessionStatus::Error);
}

// Tests that shared Codex prompt dispatch clears stale command state before notifications arrive.
#[test]
fn shared_codex_prompt_dispatch_clears_stale_command_state_before_turn_started_notification() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-prompt-dispatch-clears-stale-state");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    state
        .upsert_command_message(
            &session_id,
            "old-command-message",
            "Web search: previous turn",
            "previous turn",
            CommandStatus::Success,
        )
        .unwrap();

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                recorder: SessionRecorderState {
                    command_messages: HashMap::from([(
                        "webSearch".to_owned(),
                        "old-command-message".to_owned(),
                    )]),
                    parallel_agents_messages: HashMap::new(),
                    streaming_text_message_id: Some("stale-stream".to_owned()),
                },
                thread_id: Some("conversation-123".to_owned()),
                turn_id: Some("turn-old".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let pending_requests_for_response = pending_requests.clone();
    let response_thread = std::thread::spawn(move || {
        for _ in 0..100 {
            let sender = {
                let mut pending = pending_requests_for_response
                    .lock()
                    .expect("Codex pending requests mutex poisoned");
                if let Some(request_id) = pending.keys().next().cloned() {
                    pending.remove(&request_id)
                } else {
                    None
                }
            };
            if let Some(sender) = sender {
                let _ = sender.send(Ok(json!({
                    "turn": {
                        "id": "turn-new"
                    }
                })));
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("turn/start request should be pending");
    });

    let mut writer = Vec::new();
    // The session already has a thread_id so the fast path is taken and
    // input_tx is unused, but the parameter is still required.
    let (dummy_input_tx, _dummy_input_rx) = mpsc::channel::<CodexRuntimeCommand>();
    handle_shared_codex_prompt_command(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &runtime.thread_sessions,
        &dummy_input_tx,
        &session_id,
        CodexPromptCommand {
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "check the repo".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .unwrap();
    response_thread
        .join()
        .expect("turn/start response thread should join cleanly");

    let item_started = json!({
        "method": "item/started",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "type": "webSearch",
                "query": "serde_json value",
                "action": {
                    "type": "search",
                    "queries": ["serde_json value"]
                }
            }
        }
    });
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-new"
            }
        }
    });
    let item_completed = json!({
        "method": "item/completed",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "type": "webSearch",
                "query": "serde_json value",
                "action": {
                    "type": "search",
                    "queries": ["serde_json value"]
                }
            }
        }
    });

    handle_shared_codex_app_server_message(
        &item_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &item_completed,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    let command_messages = session
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Command {
                command,
                output,
                status,
                ..
            } => Some((command.clone(), output.clone(), *status)),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        command_messages,
        vec![
            (
                "Web search: previous turn".to_owned(),
                "previous turn".to_owned(),
                CommandStatus::Success,
            ),
            (
                "Web search: serde_json value".to_owned(),
                "serde_json value".to_owned(),
                CommandStatus::Success,
            ),
        ]
    );
}

// Tests that a fast turn/started notification cannot restore stale pending turn state.
#[test]
fn shared_codex_turn_started_notification_does_not_restore_pending_state() {
    struct RaceWriter<F: FnMut()> {
        buffer: Vec<u8>,
        injected: bool,
        on_turn_start_written: F,
    }

    impl<F: FnMut()> std::io::Write for RaceWriter<F> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.buffer.extend_from_slice(buf);
            if !self.injected && self.buffer.ends_with(b"\n") {
                let line = std::str::from_utf8(&self.buffer)
                    .expect("turn/start payload should stay valid UTF-8");
                if line.contains("\"method\":\"turn/start\"") {
                    self.injected = true;
                    (self.on_turn_start_written)();
                }
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-turn-start-race");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let callback_state = state.clone();
    let callback_pending_requests = pending_requests.clone();
    let callback_sessions = runtime.sessions.clone();
    let callback_thread_sessions = runtime.thread_sessions.clone();
    let callback_runtime_id = runtime.runtime_id.clone();
    let (callback_input_tx, _callback_input_rx) = mpsc::channel::<CodexRuntimeCommand>();
    let mut writer = RaceWriter {
        buffer: Vec::new(),
        injected: false,
        on_turn_start_written: move || {
            handle_shared_codex_app_server_message(
                &json!({
                    "method": "turn/started",
                    "params": {
                        "threadId": "conversation-123",
                        "turn": {
                            "id": "turn-fast"
                        }
                    }
                }),
                &callback_state,
                &callback_runtime_id,
                &callback_pending_requests,
                &callback_sessions,
                &callback_thread_sessions,
                &callback_input_tx,
            )
            .expect("turn/started callback should be handled");
        },
    };

    handle_shared_codex_start_turn(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &session_id,
        "conversation-123",
        CodexPromptCommand {
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "inspect race handling".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .unwrap();

    {
        let sessions = runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        let session_state = sessions
            .get(&session_id)
            .expect("shared Codex session state should exist");
        assert_eq!(session_state.turn_id.as_deref(), Some("turn-fast"));
        assert!(session_state.turn_started);
        assert_eq!(session_state.pending_turn_start_request_id, None);
    }

    let (_request_id, sender) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));
    sender
        .send(Ok(json!({
            "turn": {
                "id": "turn-fast"
            }
        })))
        .unwrap();
}

// Tests that a failed StartTurnAfterSetup handoff rolls back provisional thread registration.
#[test]
fn shared_codex_thread_setup_handoff_failure_rolls_back_registration() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _runtime_input_rx, process) =
        test_shared_codex_runtime("shared-codex-thread-setup-handoff-failure");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = Vec::new();
    let (input_tx, input_rx) = mpsc::channel::<CodexRuntimeCommand>();
    drop(input_rx);

    handle_shared_codex_prompt_command(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &runtime.thread_sessions,
        &input_tx,
        &session_id,
        CodexPromptCommand {
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "start the turn".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .unwrap();

    let (_request_id, sender) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));
    sender
        .send(Ok(json!({
            "thread": {
                "id": "conversation-orphan"
            }
        })))
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let (runtime_cleared, external_session_id, status, preview) = {
            let inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&session_id)
                .expect("Codex session should exist");
            let record = &inner.sessions[index];
            (
                matches!(record.runtime, SessionRuntime::None),
                record.external_session_id.clone(),
                record.session.status,
                record.session.preview.clone(),
            )
        };
        let (shared_thread_id, has_thread_mapping) = {
            let sessions = runtime
                .sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            let thread_id = sessions
                .get(&session_id)
                .and_then(|session| session.thread_id.clone());
            drop(sessions);
            let thread_sessions = runtime
                .thread_sessions
                .lock()
                .expect("shared Codex thread mutex poisoned");
            (
                thread_id,
                thread_sessions.contains_key("conversation-orphan"),
            )
        };

        if runtime_cleared
            && external_session_id.is_none()
            && shared_thread_id.is_none()
            && !has_thread_mapping
        {
            assert_eq!(status, SessionStatus::Error);
            assert!(preview.contains("failed to queue shared Codex turn/start after thread setup"));
            break;
        }

        assert!(
            std::time::Instant::now() < deadline,
            "failed StartTurnAfterSetup handoff should roll back provisional thread registration"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

// Tests that thread-registration persistence failures do not tear down the shared runtime.
#[test]
fn shared_codex_thread_setup_persist_failure_does_not_tear_down_runtime() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _runtime_input_rx, process) =
        test_shared_codex_runtime("shared-codex-thread-setup-persist-failure");
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-shared-codex-thread-setup-persist-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    state.persist_tx = mpsc::channel().0;

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = Vec::new();
    let (input_tx, input_rx) = mpsc::channel::<CodexRuntimeCommand>();

    handle_shared_codex_prompt_command(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &runtime.thread_sessions,
        &input_tx,
        &session_id,
        CodexPromptCommand {
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "start the turn".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .unwrap();

    let (_request_id, sender) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));
    sender
        .send(Ok(json!({
            "thread": {
                "id": "conversation-persist-failure"
            }
        })))
        .unwrap();

    std::thread::sleep(Duration::from_millis(50));
    assert!(
        matches!(
            input_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "failed thread registration should not queue StartTurnAfterSetup"
    );

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        assert!(matches!(
            &inner.sessions[index].runtime,
            SessionRuntime::Codex(handle) if handle.runtime_id == runtime.runtime_id
        ));
    }
    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.status, SessionStatus::Error);
    assert!(
        session.preview.contains("Failed to save session state"),
        "persistence-failure preview should use generic message, got: {}",
        session.preview,
    );
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. })
            if text.contains("Turn failed: Failed to save session state")
    ));
    assert!(
        !runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("conversation-persist-failure"),
        "failed thread registration should not publish a shared thread mapping"
    );

    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Tests that stale StartTurnAfterSetup callbacks do not persist runtime config onto another runtime.
#[test]
fn shared_codex_stale_start_turn_handoff_skips_runtime_config_persistence() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (other_runtime, _other_input_rx) = test_codex_runtime_handle("other-runtime");
    let sessions = SharedCodexSessions::new();
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = Vec::new();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(other_runtime);
        inner.sessions[index].active_codex_approval_policy = None;
        inner.sessions[index].active_codex_reasoning_effort = None;
        inner.sessions[index].active_codex_sandbox_mode = None;
    }

    handle_shared_codex_start_turn(
        &mut writer,
        &pending_requests,
        &state,
        "stale-runtime",
        &sessions,
        &session_id,
        "conversation-stale",
        CodexPromptCommand {
            approval_policy: CodexApprovalPolicy::OnRequest,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "stale handoff".to_owned(),
            reasoning_effort: CodexReasoningEffort::XHigh,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::DangerFullAccess,
        },
    )
    .unwrap();

    assert!(writer.is_empty());
    assert!(
        pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned")
            .is_empty()
    );
    assert!(
        sessions
            .lock()
            .expect("shared Codex session mutex poisoned")
            .is_empty()
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&session_id)
        .expect("Codex session should exist");
    assert_eq!(inner.sessions[index].active_codex_approval_policy, None);
    assert_eq!(inner.sessions[index].active_codex_reasoning_effort, None);
    assert_eq!(inner.sessions[index].active_codex_sandbox_mode, None);
}

// Tests that external session id setup distinguishes persistence failures from stale-session skips.
#[test]
fn set_external_session_id_if_runtime_matches_reports_persist_failure() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx) = test_codex_runtime_handle("persist-thread-id-runtime");
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-codex-thread-id-persist-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    state.persist_tx = mpsc::channel().0;

    let result = state.set_external_session_id_if_runtime_matches(
        &session_id,
        &RuntimeToken::Codex("persist-thread-id-runtime".to_owned()),
        "conversation-123".to_owned(),
    );

    assert!(
        result.is_err(),
        "commit failures should not collapse into stale-session misses"
    );
    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Tests that runtime config setup distinguishes persistence failures from stale-session skips.
#[test]
fn record_codex_runtime_config_if_runtime_matches_reports_persist_failure() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx) = test_codex_runtime_handle("persist-runtime-config-runtime");
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-codex-runtime-config-persist-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    state.persist_tx = mpsc::channel().0;

    let result = state.record_codex_runtime_config_if_runtime_matches(
        &session_id,
        &RuntimeToken::Codex("persist-runtime-config-runtime".to_owned()),
        CodexSandboxMode::WorkspaceWrite,
        CodexApprovalPolicy::Never,
        CodexReasoningEffort::Medium,
    );

    assert!(
        result.is_err(),
        "persistence failures should remain fatal to the caller"
    );
    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Tests that shared Codex runtime-config persistence failures stay session-scoped.
#[test]
fn shared_codex_start_turn_persist_failure_does_not_tear_down_runtime() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _runtime_input_rx, process) =
        test_shared_codex_runtime("shared-codex-start-turn-persist-failure");
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-shared-codex-start-turn-persist-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    state.persist_tx = mpsc::channel().0;

    handle_shared_codex_start_turn(
        &mut Vec::new(),
        &Arc::new(Mutex::new(HashMap::new())),
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &session_id,
        "conversation-123",
        CodexPromptCommand {
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "start the turn".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .unwrap();

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&session_id)
        .expect("Codex session should exist");
    assert!(matches!(
        &inner.sessions[index].runtime,
        SessionRuntime::Codex(handle) if handle.runtime_id == runtime.runtime_id
    ));
    drop(inner);

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.status, SessionStatus::Error);
    assert!(
        session.preview.contains("Failed to save session state"),
        "persistence-failure preview should use generic message, got: {}",
        session.preview,
    );
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. })
            if text.contains("Turn failed: Failed to save session state")
    ));

    let _ = fs::remove_dir_all(failing_persistence_path);
}

#[test]
fn shared_codex_event_matches_visible_turn_handles_active_and_completed_turns() {
    assert!(shared_codex_event_matches_visible_turn(
        Some("turn-active"),
        None,
        Some("turn-active"),
    ));
    assert!(shared_codex_event_matches_visible_turn(
        None,
        Some("turn-completed"),
        Some("turn-completed"),
    ));
    assert!(!shared_codex_event_matches_visible_turn(
        None,
        Some("turn-completed"),
        Some("turn-other"),
    ));
    assert!(!shared_codex_event_matches_visible_turn(
        None,
        Some("turn-completed"),
        None,
    ));
    // No active or completed turn — event with a turn ID is rejected.
    assert!(!shared_codex_event_matches_visible_turn(
        None,
        None,
        Some("turn-orphan"),
    ));
    // Active turn differs from event but completed turn matches — the
    // completed branch is only entered when current_turn_id is None.
    assert!(!shared_codex_event_matches_visible_turn(
        Some("turn-active"),
        Some("turn-completed"),
        Some("turn-completed"),
    ));
}

#[test]
fn shared_codex_app_server_error_classifier_only_ignores_missing_sessions() {
    assert!(shared_codex_app_server_error_is_stale_session(&anyhow!(
        "session `session-1` not found"
    )));
    assert!(shared_codex_app_server_error_is_stale_session(
        &anyhow!("session `session-1` not found").context("wrapped")
    ));
    assert!(!shared_codex_app_server_error_is_stale_session(&anyhow!(
        "session `session-1` message `message-1` not found"
    )));
    assert!(!shared_codex_app_server_error_is_stale_session(
        &anyhow!("session `session-1` message `message-1` not found").context("wrapped")
    ));
    assert!(!shared_codex_app_server_error_is_stale_session(&anyhow!(
        "session `session-1` anchor message `message-1` not found"
    )));
    assert!(!shared_codex_app_server_error_is_stale_session(&anyhow!(
        "failed to persist Codex notice"
    )));
}

#[test]
fn json_rpc_request_message_includes_jsonrpc() {
    assert_eq!(
        json_rpc_request_message(
            "request-1".to_owned(),
            "model/list",
            json!({
                "limit": 100,
            }),
        ),
        json!({
            "jsonrpc": "2.0",
            "id": "request-1",
            "method": "model/list",
            "params": {
                "limit": 100,
            }
        })
    );
}

#[test]
fn json_rpc_notification_message_includes_jsonrpc() {
    assert_eq!(
        json_rpc_notification_message("initialized"),
        json!({
            "jsonrpc": "2.0",
            "method": "initialized",
        })
    );
}

#[test]
fn codex_json_rpc_response_message_includes_jsonrpc_for_result_payload() {
    let response = CodexJsonRpcResponseCommand {
        request_id: json!("request-ok"),
        payload: CodexJsonRpcResponsePayload::Result(json!({
            "decision": "accept",
        })),
    };

    assert_eq!(
        codex_json_rpc_response_message(&response),
        json!({
            "jsonrpc": "2.0",
            "id": "request-ok",
            "result": {
                "decision": "accept",
            }
        })
    );
}

#[test]
fn codex_json_rpc_response_message_includes_jsonrpc_for_error_payload() {
    let response = CodexJsonRpcResponseCommand {
        request_id: json!("request-error"),
        payload: CodexJsonRpcResponsePayload::Error {
            code: -32001,
            message: "Session unavailable; request could not be delivered.".to_owned(),
        },
    };

    assert_eq!(
        codex_json_rpc_response_message(&response),
        json!({
            "jsonrpc": "2.0",
            "id": "request-error",
            "error": {
                "code": -32001,
                "message": "Session unavailable; request could not be delivered.",
            }
        })
    );
}

// Tests that undeliverable shared Codex server requests return a protocol-valid JSON-RPC error.
#[test]
fn shared_codex_undeliverable_server_request_returns_json_rpc_error() {
    let state = test_app_state();
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let sessions = SharedCodexSessions::new();
    let thread_sessions: SharedCodexThreadMap = Arc::new(Mutex::new(HashMap::new()));
    let (input_tx, input_rx) = mpsc::channel::<CodexRuntimeCommand>();

    handle_shared_codex_app_server_message(
        &json!({
            "jsonrpc": "2.0",
            "id": "request-missing-session",
            "method": "session/request_permission",
            "params": {
                "threadId": "missing-thread"
            }
        }),
        &state,
        "shared-codex-missing-session",
        &pending_requests,
        &sessions,
        &thread_sessions,
        &input_tx,
    )
    .unwrap();

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        CodexRuntimeCommand::JsonRpcResponse { response } => {
            assert_eq!(
                codex_json_rpc_response_message(&response),
                json!({
                    "jsonrpc": "2.0",
                    "id": "request-missing-session",
                    "error": {
                        "code": -32001,
                        "message": "Session unavailable; request could not be delivered."
                    }
                })
            );
        }
        _ => panic!("expected JSON-RPC rejection"),
    }
}

// Tests that shared Codex prompt dispatch keeps the writer loop responsive while turn/start waits.
#[test]
fn shared_codex_prompt_command_keeps_writer_loop_responsive_while_turn_start_is_pending() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _runtime_input_rx, process) =
        test_shared_codex_runtime("shared-codex-prompt-writer-responsive");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let writer = SharedBufferWriter::default();
    let thread_writer = writer.clone();
    let thread_pending_requests = pending_requests.clone();
    let thread_state = state.clone();
    let thread_runtime = runtime.clone();
    let (input_tx, input_rx) = mpsc::channel();
    let thread_input_tx = input_tx.clone();

    let writer_thread = std::thread::spawn(move || {
        let mut stdin = thread_writer;
        let runtime_token = RuntimeToken::Codex(thread_runtime.runtime_id.clone());
        while let Ok(command) = input_rx.recv_timeout(Duration::from_millis(250)) {
            match command {
                CodexRuntimeCommand::Prompt {
                    session_id,
                    command,
                } => {
                    handle_shared_codex_prompt_command_result(
                        &thread_state,
                        &session_id,
                        &runtime_token,
                        handle_shared_codex_prompt_command(
                            &mut stdin,
                            &thread_pending_requests,
                            &thread_state,
                            &thread_runtime.runtime_id,
                            &thread_runtime.sessions,
                            &thread_runtime.thread_sessions,
                            &thread_input_tx,
                            &session_id,
                            command,
                        ),
                    )
                    .unwrap();
                }
                CodexRuntimeCommand::StartTurnAfterSetup {
                    session_id,
                    thread_id,
                    command,
                } => {
                    handle_shared_codex_prompt_command_result(
                        &thread_state,
                        &session_id,
                        &runtime_token,
                        handle_shared_codex_start_turn(
                            &mut stdin,
                            &thread_pending_requests,
                            &thread_state,
                            &thread_runtime.runtime_id,
                            &thread_runtime.sessions,
                            &session_id,
                            &thread_id,
                            command,
                        ),
                    )
                    .unwrap();
                }
                CodexRuntimeCommand::JsonRpcResponse { response } => {
                    write_codex_json_rpc_message(
                        &mut stdin,
                        &codex_json_rpc_response_message(&response),
                    )
                    .unwrap();
                }
                _ => panic!("unexpected shared Codex runtime command"),
            }
        }
    });

    input_tx
        .send(CodexRuntimeCommand::Prompt {
            session_id: session_id.clone(),
            command: CodexPromptCommand {
                approval_policy: CodexApprovalPolicy::Never,
                attachments: Vec::new(),
                cwd: "/tmp".to_owned(),
                model: "gpt-5.4".to_owned(),
                prompt: "check the repo".to_owned(),
                reasoning_effort: CodexReasoningEffort::Medium,
                resume_thread_id: None,
                sandbox_mode: CodexSandboxMode::WorkspaceWrite,
            },
        })
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let written = writer.contents();
        let pending_count = pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned")
            .len();
        if written.contains("\"method\":\"turn/start\"") && pending_count == 1 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "turn/start request should stay pending while the writer loop remains active"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    input_tx
        .send(CodexRuntimeCommand::JsonRpcResponse {
            response: CodexJsonRpcResponseCommand {
                request_id: json!("approval-1"),
                payload: CodexJsonRpcResponsePayload::Result(json!({
                    "outcome": "approved",
                })),
            },
        })
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let written = writer.contents();
        if written.contains("\"id\":\"approval-1\"") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "writer loop should still write JSON-RPC responses while turn/start is pending"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let (_request_id, sender) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));
    sender
        .send(Ok(json!({
            "turn": {
                "id": "turn-1"
            }
        })))
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let (turn_id, pending_turn_start) = {
            let sessions = runtime
                .sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            let session_state = sessions
                .get(&session_id)
                .expect("shared Codex session state should exist");
            (
                session_state.turn_id.clone(),
                session_state.pending_turn_start_request_id.clone(),
            )
        };
        if turn_id.as_deref() == Some("turn-1") && pending_turn_start.is_none() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "turn/start waiter should record the turn id after the response arrives"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    drop(input_tx);
    writer_thread
        .join()
        .expect("shared Codex writer thread should join cleanly");
}

// Tests that shared Codex prompt JSON-RPC errors fail the turn without tearing down the runtime.
#[test]
fn shared_codex_prompt_json_rpc_errors_fail_the_turn_without_tearing_down_runtime() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-prompt-jsonrpc-error");
    let runtime_token = RuntimeToken::Codex(runtime.runtime_id.clone());

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Waiting for Codex".to_owned();
    }

    handle_shared_codex_prompt_command_result(
        &state,
        &session_id,
        &runtime_token,
        Err(anyhow::Error::new(CodexResponseError::JsonRpc(
            "turn/start rejected the request".to_owned(),
        ))),
    )
    .expect("JSON-RPC prompt errors should be recorded as turn failures");

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.status, SessionStatus::Error);
    assert_eq!(session.preview, "turn/start rejected the request");
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. })
            if text == "Turn failed: turn/start rejected the request"
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert!(matches!(record.runtime, SessionRuntime::Codex(_)));
}

// Tests that shared Codex app-server agent deltas wait for turn started.
#[test]
fn shared_codex_app_server_agent_message_delta_waits_for_turn_started() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-app-server-agent-delta-turn-started");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                turn_id: Some("turn-current".to_owned()),
                turn_started: false,
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let delta = json!({
        "method": "item/agentMessage/delta",
        "params": {
            "threadId": "conversation-123",
            "itemId": "msg-1",
            "delta": "Hello"
        }
    });
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-current"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &delta,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &delta,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.preview, "Hello");
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Hello"
    ));
}

// Tests that shared Codex app-server requests wait for turn started.
#[test]
fn shared_codex_app_server_request_waits_for_turn_started() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-app-server-request-turn-started");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                turn_id: Some("turn-current".to_owned()),
                turn_started: false,
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let request = json!({
        "id": "req-1",
        "method": "item/tool/requestUserInput",
        "params": {
            "threadId": "conversation-123",
            "questions": [
                {
                    "header": "Scope",
                    "id": "scope",
                    "question": "What should Codex review?"
                }
            ]
        }
    });
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-current"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &request,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &request,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(matches!(
        session.messages.last(),
        Some(Message::UserInputRequest { title, detail, state, .. })
            if title == "Codex needs input"
                && detail == "Codex requested additional input for \"Scope\"."
                && *state == InteractionRequestState::Pending
    ));
}

// Tests that Codex app server command approval request records pending approval.
#[test]
fn codex_app_server_command_approval_request_records_pending_approval() {
    let mut recorder = TestRecorder::default();
    let message = json!({
        "id": "req-1",
        "params": {
            "command": "cargo test",
            "cwd": "/tmp/project",
            "reason": "Need to verify the fix."
        }
    });

    handle_codex_app_server_request(
        "item/commandExecution/requestApproval",
        &message,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(recorder.codex_approvals.len(), 1);
    assert!(matches!(
        recorder.codex_approvals.first(),
        Some((title, command, detail, approval))
            if title == "Codex needs approval"
                && command == "cargo test"
                && detail == "Codex requested approval to execute this command in /tmp/project. Reason: Need to verify the fix."
                && matches!(approval.kind, CodexApprovalKind::CommandExecution)
                && approval.request_id == json!("req-1")
    ));
}

// Tests that Codex app server file change approval request records pending approval.
#[test]
fn codex_app_server_file_change_approval_request_records_pending_approval() {
    let mut recorder = TestRecorder::default();
    let message = json!({
        "id": "req-2",
        "params": {
            "reason": "Need to update generated files."
        }
    });

    handle_codex_app_server_request("item/fileChange/requestApproval", &message, &mut recorder)
        .unwrap();

    assert_eq!(recorder.codex_approvals.len(), 1);
    assert!(matches!(
        recorder.codex_approvals.first(),
        Some((title, command, detail, approval))
            if title == "Codex needs approval"
                && command == "Apply file changes"
                && detail == "Codex requested approval to apply file changes. Reason: Need to update generated files."
                && matches!(approval.kind, CodexApprovalKind::FileChange)
                && approval.request_id == json!("req-2")
    ));
}

// Tests that Codex app server permissions approval request records pending approval.
#[test]
fn codex_app_server_permissions_approval_request_records_pending_approval() {
    let mut recorder = TestRecorder::default();
    let requested_permissions = json!({
        "fileSystem": {
            "read": ["/repo/docs"],
            "write": ["/repo/src"]
        },
        "network": {
            "enabled": true
        },
        "macos": {
            "preferences": "system",
            "automations": {
                "bundle_ids": ["com.apple.Terminal"]
            }
        }
    });
    let message = json!({
        "id": "req-3",
        "params": {
            "permissions": requested_permissions,
            "reason": "Need access to update build scripts."
        }
    });

    handle_codex_app_server_request("item/permissions/requestApproval", &message, &mut recorder)
        .unwrap();

    assert_eq!(recorder.codex_approvals.len(), 1);
    let (title, command, detail, approval) = recorder
        .codex_approvals
        .first()
        .expect("Codex permissions approval should be recorded");
    assert_eq!(title, "Codex needs approval");
    assert_eq!(command, "Grant additional permissions");
    assert_eq!(
        detail,
        "Codex requested approval to grant additional permissions: read access to `/repo/docs`, write access to `/repo/src`, network access, macOS preferences access (system), macOS automation access for `com.apple.Terminal`. Reason: Need access to update build scripts."
    );
    match &approval.kind {
        CodexApprovalKind::Permissions {
            requested_permissions,
        } => {
            assert_eq!(
                requested_permissions,
                &json!({
                    "fileSystem": {
                        "read": ["/repo/docs"],
                        "write": ["/repo/src"]
                    },
                    "network": {
                        "enabled": true
                    },
                    "macos": {
                        "preferences": "system",
                        "automations": {
                            "bundle_ids": ["com.apple.Terminal"]
                        }
                    }
                })
            );
        }
        _ => panic!("expected Codex permissions approval"),
    }
    assert_eq!(approval.request_id, json!("req-3"));
}

// Tests that Codex app server user input request records pending request.
#[test]
fn codex_app_server_user_input_request_records_pending_request() {
    let mut recorder = TestRecorder::default();
    let message = json!({
        "id": "req-input-1",
        "params": {
            "questions": [
                {
                    "header": "Environment",
                    "id": "environment",
                    "question": "Which environment should I use?",
                    "options": [
                        {
                            "label": "Production",
                            "description": "Use the production cluster."
                        },
                        {
                            "label": "Staging",
                            "description": "Use the staging environment."
                        }
                    ]
                },
                {
                    "header": "API token",
                    "id": "apiToken",
                    "question": "Paste the temporary token.",
                    "isSecret": true
                }
            ]
        }
    });

    handle_codex_app_server_request("item/tool/requestUserInput", &message, &mut recorder).unwrap();

    assert_eq!(recorder.codex_user_input_requests.len(), 1);
    let (title, detail, questions, request) = recorder
        .codex_user_input_requests
        .first()
        .expect("Codex user input request should be recorded");
    assert_eq!(title, "Codex needs input");
    assert_eq!(detail, "Codex requested additional input for 2 questions.");
    assert_eq!(questions.len(), 2);
    assert_eq!(questions[0].header, "Environment");
    assert_eq!(questions[1].id, "apiToken");
    assert!(questions[1].is_secret);
    assert_eq!(request.request_id, json!("req-input-1"));
    assert_eq!(request.questions, questions.clone());
}

// Tests that Codex app server MCP elicitation request records pending request.
#[test]
fn codex_app_server_mcp_elicitation_request_records_pending_request() {
    let mut recorder = TestRecorder::default();
    let message = json!({
        "id": "req-elicit-1",
        "params": {
            "threadId": "thread-1",
            "turnId": "turn-1",
            "serverName": "deployment-helper",
            "mode": "form",
            "message": "Confirm the deployment settings.",
            "requestedSchema": {
                "type": "object",
                "properties": {
                    "environment": {
                        "type": "string",
                        "title": "Environment",
                        "oneOf": [
                            { "const": "production", "title": "Production" },
                            { "const": "staging", "title": "Staging" }
                        ]
                    },
                    "replicas": {
                        "type": "integer",
                        "title": "Replicas"
                    }
                },
                "required": ["environment", "replicas"]
            }
        }
    });

    handle_codex_app_server_request("mcpServer/elicitation/request", &message, &mut recorder)
        .unwrap();

    assert_eq!(recorder.codex_mcp_elicitation_requests.len(), 1);
    let (title, detail, request, pending) = recorder
        .codex_mcp_elicitation_requests
        .first()
        .expect("MCP elicitation request should be recorded");
    assert_eq!(title, "Codex needs MCP input");
    assert_eq!(
        detail,
        "MCP server deployment-helper requested additional structured input. Confirm the deployment settings."
    );
    assert_eq!(request.server_name, "deployment-helper");
    assert_eq!(request.thread_id, "thread-1");
    assert_eq!(request.turn_id.as_deref(), Some("turn-1"));
    assert!(matches!(
        request.mode,
        McpElicitationRequestMode::Form { .. }
    ));
    assert_eq!(pending.request_id, json!("req-elicit-1"));
    assert_eq!(pending.request, *request);
}

// Tests that Codex app server generic request records pending request.
#[test]
fn codex_app_server_generic_request_records_pending_request() {
    let mut recorder = TestRecorder::default();
    let message = json!({
        "id": "req-tool-1",
        "params": {
            "toolName": "search_workspace",
            "arguments": {
                "pattern": "Codex"
            }
        }
    });

    handle_codex_app_server_request("item/tool/call", &message, &mut recorder).unwrap();

    assert_eq!(recorder.codex_app_requests.len(), 1);
    let (title, detail, method, params, pending) = recorder
        .codex_app_requests
        .first()
        .expect("generic Codex app request should be recorded");
    assert_eq!(title, "Codex needs a tool result");
    assert_eq!(
        detail,
        "Codex requested a result for `search_workspace`. Review the request payload and submit the JSON result to continue."
    );
    assert_eq!(method, "item/tool/call");
    assert_eq!(
        params,
        &json!({
            "toolName": "search_workspace",
            "arguments": {
                "pattern": "Codex"
            }
        })
    );
    assert_eq!(pending.request_id, json!("req-tool-1"));
}

// Tests that REPL Codex task complete event buffers subagent result until final message.
#[test]
fn repl_codex_task_complete_event_buffers_subagent_result_until_final_message() {
    let mut recorder = TestRecorder::default();
    let mut repl_state = ReplCodexSessionState::default();
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-sub-1"
            }
        }
    });
    let task_complete = json!({
        "method": "codex/event/task_complete",
        "params": {
            "conversationId": "conversation-123",
            "msg": {
                "last_agent_message": "Reviewer found a real bug.",
                "turn_id": "turn-sub-1",
                "type": "task_complete"
            }
        }
    });
    let final_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-sub-1",
            "msg": {
                "message": "Final REPL Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_repl_codex_app_server_notification(
        "turn/started",
        &turn_started,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "codex/event/task_complete",
        &task_complete,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();

    assert!(recorder.subagent_results.is_empty());
    assert!(recorder.texts.is_empty());

    handle_repl_codex_app_server_notification(
        "codex/event/agent_message",
        &final_message,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(
        recorder.subagent_results,
        vec![(
            "Subagent completed".to_owned(),
            "Reviewer found a real bug.".to_owned(),
        )]
    );
    assert_eq!(
        recorder.texts,
        vec![
            "Subagent completed\nReviewer found a real bug.".to_owned(),
            "Final REPL Codex answer.".to_owned(),
        ]
    );
}

// Tests that REPL Codex streamed agent message appends missing completed suffix.
#[test]
fn repl_codex_streamed_agent_message_appends_missing_completed_suffix() {
    let mut recorder = TestRecorder::default();
    let mut repl_state = ReplCodexSessionState::default();
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stream-1"
            }
        }
    });
    let delta = json!({
        "method": "item/agentMessage/delta",
        "params": {
            "threadId": "conversation-123",
            "itemId": "item-1",
            "delta": "Hello"
        }
    });
    let completed = json!({
        "method": "item/completed",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "id": "item-1",
                "type": "agentMessage",
                "text": "Hello from REPL."
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stream-1",
                "error": null
            }
        }
    });

    handle_repl_codex_app_server_notification(
        "turn/started",
        &turn_started,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "item/agentMessage/delta",
        &delta,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "item/completed",
        &completed,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "turn/completed",
        &turn_completed,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(
        recorder.text_deltas,
        vec!["Hello".to_owned(), " from REPL.".to_owned()]
    );
    assert!(recorder.texts.is_empty());
    assert!(repl_state.turn_completed);
    assert!(repl_state.current_turn_id.is_none());
}

// Tests that REPL Codex streamed agent message replaces divergent completed text.
#[test]
fn repl_codex_streamed_agent_message_replaces_divergent_completed_text() {
    let mut recorder = TestRecorder::default();
    let mut repl_state = ReplCodexSessionState::default();
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stream-1"
            }
        }
    });
    let delta = json!({
        "method": "item/agentMessage/delta",
        "params": {
            "threadId": "conversation-123",
            "itemId": "item-1",
            "delta": "Hello from stream"
        }
    });
    let completed = json!({
        "method": "item/completed",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "id": "item-1",
                "type": "agentMessage",
                "text": "Different final answer."
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stream-1",
                "error": null
            }
        }
    });
    handle_repl_codex_app_server_notification(
        "turn/started",
        &turn_started,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "item/agentMessage/delta",
        &delta,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "item/completed",
        &completed,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "turn/completed",
        &turn_completed,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    assert_eq!(
        recorder.text_deltas,
        vec!["Different final answer.".to_owned()]
    );
    assert!(recorder.texts.is_empty());
    assert!(repl_state.turn_completed);
    assert!(repl_state.current_turn_id.is_none());
}
// Tests that REPL Codex streamed agent message skips duplicate completed text.
#[test]
fn repl_codex_streamed_agent_message_skips_duplicate_completed_text() {
    let mut recorder = TestRecorder::default();
    let mut repl_state = ReplCodexSessionState::default();
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stream-1"
            }
        }
    });
    let delta = json!({
        "method": "item/agentMessage/delta",
        "params": {
            "threadId": "conversation-123",
            "itemId": "item-1",
            "delta": "Hello from REPL."
        }
    });
    let completed = json!({
        "method": "item/completed",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "id": "item-1",
                "type": "agentMessage",
                "text": "Hello from REPL."
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stream-1",
                "error": null
            }
        }
    });

    handle_repl_codex_app_server_notification(
        "turn/started",
        &turn_started,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "item/agentMessage/delta",
        &delta,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "item/completed",
        &completed,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "turn/completed",
        &turn_completed,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(recorder.text_deltas, vec!["Hello from REPL.".to_owned()]);
    assert!(recorder.texts.is_empty());
    assert!(repl_state.turn_completed);
    assert!(repl_state.current_turn_id.is_none());
}

// Tests that REPL Codex final agent messages still land after turn completion.
#[test]
fn repl_codex_agent_message_after_turn_completed_is_recorded() {
    let mut recorder = TestRecorder::default();
    let mut repl_state = ReplCodexSessionState::default();
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-finished"
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-finished",
                "error": null
            }
        }
    });
    let late_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-finished",
            "msg": {
                "message": "Late REPL answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_repl_codex_app_server_notification(
        "turn/started",
        &turn_started,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "turn/completed",
        &turn_completed,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "codex/event/agent_message",
        &late_message,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(recorder.texts, vec!["Late REPL answer.".to_owned()]);
    assert!(repl_state.turn_completed);
    assert!(repl_state.current_turn_id.is_none());
}

// Tests that REPL Codex app-server agentMessage completions still land after turn completion.
#[test]
fn repl_codex_app_server_agent_message_completed_after_turn_completed_is_recorded() {
    let mut recorder = TestRecorder::default();
    let mut repl_state = ReplCodexSessionState::default();
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-finished"
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-finished",
                "error": null
            }
        }
    });
    let late_item = json!({
        "method": "item/completed",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "id": "item-final",
                "type": "agentMessage",
                "text": "Late REPL item answer."
            }
        }
    });

    handle_repl_codex_app_server_notification(
        "turn/started",
        &turn_started,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "turn/completed",
        &turn_completed,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "item/completed",
        &late_item,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(recorder.texts, vec!["Late REPL item answer.".to_owned()]);
    assert!(repl_state.turn_completed);
    assert!(repl_state.current_turn_id.is_none());
}

// Tests that Codex app server web search item records command lifecycle.
#[test]
fn codex_app_server_web_search_item_records_command_lifecycle() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let mut recorder = TestRecorder::default();
    let mut turn_state = CodexTurnState::default();
    let item = json!({
        "id": "web-1",
        "type": "webSearch",
        "query": "rust anyhow",
        "action": {
            "type": "search",
            "queries": ["rust anyhow", "serde_json value"]
        }
    });

    handle_codex_app_server_item_started(&item, &mut recorder).unwrap();
    handle_codex_app_server_item_completed(
        &item,
        &state,
        &session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(
        recorder.commands,
        vec![
            (
                "Web search: rust anyhow".to_owned(),
                String::new(),
                CommandStatus::Running,
            ),
            (
                "Web search: rust anyhow".to_owned(),
                "rust anyhow\nserde_json value".to_owned(),
                CommandStatus::Success,
            ),
        ]
    );
}

// Tests that Codex app server file change item records create and edit diffs.
#[test]
fn codex_app_server_file_change_item_records_create_and_edit_diffs() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let mut recorder = TestRecorder::default();
    let mut turn_state = CodexTurnState::default();
    let item = json!({
        "type": "fileChange",
        "status": "completed",
        "changes": [
            {
                "path": "src/new.rs",
                "diff": "+fn main() {}\n",
                "kind": {
                    "type": "add"
                }
            },
            {
                "path": "src/lib.rs",
                "diff": "@@ -1 +1 @@\n-old\n+new\n",
                "kind": {
                    "type": "edit"
                }
            }
        ]
    });

    handle_codex_app_server_item_completed(
        &item,
        &state,
        &session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(
        recorder.diffs,
        vec![
            (
                "src/new.rs".to_owned(),
                "Created new.rs".to_owned(),
                "+fn main() {}\n".to_owned(),
                ChangeType::Create,
            ),
            (
                "src/lib.rs".to_owned(),
                "Updated lib.rs".to_owned(),
                "@@ -1 +1 @@\n-old\n+new\n".to_owned(),
                ChangeType::Edit,
            ),
        ]
    );
}

// Tests that Codex delta suffix deduplicates cumulative and overlapping chunks.
#[test]
fn codex_delta_suffix_deduplicates_cumulative_and_overlapping_chunks() {
    let mut text = String::new();

    assert_eq!(
        next_codex_delta_suffix(&mut text, "Try"),
        Some("Try".to_owned())
    );
    assert_eq!(text, "Try");

    assert_eq!(
        next_codex_delta_suffix(&mut text, "Try these"),
        Some(" these".to_owned())
    );
    assert_eq!(text, "Try these");

    assert_eq!(next_codex_delta_suffix(&mut text, "Try these"), None);
    assert_eq!(text, "Try these");

    assert_eq!(
        next_codex_delta_suffix(&mut text, " these plain"),
        Some(" plain".to_owned())
    );
    assert_eq!(text, "Try these plain");

    assert_eq!(next_codex_delta_suffix(&mut text, " plain"), None);
    assert_eq!(text, "Try these plain");
}

// Tests that Codex delta suffix handles multibyte UTF-8 characters.
#[test]
fn codex_delta_suffix_handles_multibyte_utf8_characters() {
    let mut text = String::new();

    // Smart quote ' is 3 bytes (U+2018: E2 80 98)
    assert_eq!(
        next_codex_delta_suffix(&mut text, "I\u{2018}m"),
        Some("I\u{2018}m".to_owned())
    );
    assert_eq!(text, "I\u{2018}m");

    // Overlapping chunk that shares the multi-byte char boundary
    assert_eq!(
        next_codex_delta_suffix(&mut text, "\u{2018}m here"),
        Some(" here".to_owned())
    );
    assert_eq!(text, "I\u{2018}m here");
}

// Tests that shared Codex agent message event uses conversation ID for session routing.
#[test]
fn shared_codex_agent_message_event_uses_conversation_id_for_session_routing() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-agent-final");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-123"
            }
        }
    });
    let message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "message": "Final shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Final shared Codex answer."
    ));
}

// Tests that subagent results append after existing assistant text.
#[test]
fn subagent_results_append_after_existing_assistant_text() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);

    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: "assistant-1".to_owned(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Final answer".to_owned(),
                expanded_text: None,
            },
        )
        .unwrap();
    state
        .push_message(
            &session_id,
            Message::SubagentResult {
                id: "subagent-1".to_owned(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Subagent completed".to_owned(),
                summary: "Hidden thinking".to_owned(),
                conversation_id: None,
                turn_id: None,
            },
        )
        .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should be present");

    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Final answer"
    ));
    assert!(matches!(
        session.messages.last(),
        Some(Message::SubagentResult { .. })
    ));
}

// Tests that clear runtime commits revision when it resets state.
#[test]
fn clear_runtime_commits_revision_when_it_resets_state() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, _input_rx) = test_claude_runtime_handle("clear-runtime");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].runtime_reset_required = true;
        state.commit_locked(&mut inner).unwrap();
    }

    let baseline = state.snapshot().revision;
    state.clear_runtime(&session_id).unwrap();

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        let record = &inner.sessions[index];
        assert!(matches!(record.runtime, SessionRuntime::None));
        assert!(!record.runtime_reset_required);
    }
    assert_eq!(state.snapshot().revision, baseline + 1);

    let stable_revision = state.snapshot().revision;
    state.clear_runtime(&session_id).unwrap();
    assert_eq!(state.snapshot().revision, stable_revision);
}

// Tests that reuses shared Codex runtime across sessions.
#[test]
fn reuses_shared_codex_runtime_across_sessions() {
    let state = test_app_state();
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime.clone());

    let first = spawn_codex_runtime(state.clone(), "session-a".to_owned(), "/tmp".to_owned())
        .expect("first Codex handle should attach");
    let second = spawn_codex_runtime(state.clone(), "session-b".to_owned(), "/tmp".to_owned())
        .expect("second Codex handle should attach");

    assert_eq!(first.runtime_id, "shared-codex");
    assert_eq!(second.runtime_id, "shared-codex");
    assert!(Arc::ptr_eq(&first.process, &process));
    assert!(Arc::ptr_eq(&second.process, &process));
    let shared_sessions = runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    assert!(!shared_sessions.contains_key("session-a"));
    assert!(!shared_sessions.contains_key("session-b"));
    assert!(shared_sessions.is_empty());
}

// Tests that stops shared Codex sessions via turn interrupt.
#[test]
fn stops_shared_codex_sessions_via_turn_interrupt() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) = test_shared_codex_runtime("shared-codex-stop");
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-123".to_owned()),
                turn_id: Some("turn-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("thread-123".to_owned(), session_id.clone());

    let handle = CodexRuntimeHandle {
        runtime_id: runtime.runtime_id.clone(),
        input_tx: runtime.input_tx.clone(),
        process,
        shared_session: Some(SharedCodexSessionHandle {
            runtime: runtime.clone(),
            session_id: session_id.clone(),
        }),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(handle);
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Codex interrupt command should arrive");
        match command {
            CodexRuntimeCommand::InterruptTurn {
                thread_id,
                turn_id,
                response_tx,
            } => {
                assert_eq!(thread_id, "thread-123");
                assert_eq!(turn_id, "turn-123");
                let _ = response_tx.send(Ok(()));
            }
            _ => panic!("expected Codex turn interrupt command"),
        }
    });

    let snapshot = state.stop_session(&session_id).unwrap();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("stopped session should remain present");
    assert_eq!(session.status, SessionStatus::Idle);
    assert!(session.external_session_id.is_none());
    assert!(session.codex_thread_state.is_none());
    assert!(session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn stopped by user."
    )));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert!(matches!(record.runtime, SessionRuntime::None));
    drop(inner);

    let shared_sessions = runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    assert!(!shared_sessions.contains_key(&session_id));
    drop(shared_sessions);
    assert!(
        !runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("thread-123")
    );
}

// Tests that stop session detaches shared Codex session when interrupt fails.
#[test]
fn stop_session_detaches_shared_codex_session_when_interrupt_fails() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = SharedCodexRuntime {
        runtime_id: "shared-codex-stop-fail".to_owned(),
        input_tx,
        process: process.clone(),
        sessions: SharedCodexSessions::new(),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    drop(input_rx);

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-stop-fail".to_owned()),
                turn_id: Some("turn-stop-fail".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("thread-stop-fail".to_owned(), session_id.clone());

    let handle = CodexRuntimeHandle {
        runtime_id: runtime.runtime_id.clone(),
        input_tx: runtime.input_tx.clone(),
        process: process.clone(),
        shared_session: Some(SharedCodexSessionHandle {
            runtime: runtime.clone(),
            session_id: session_id.clone(),
        }),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(handle);
        inner.sessions[index].session.status = SessionStatus::Active;
        set_record_external_session_id(
            &mut inner.sessions[index],
            Some("thread-stop-fail".to_owned()),
        );
    }

    let snapshot = state.stop_session(&session_id).unwrap();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("stopped session should remain present");
    assert_eq!(session.status, SessionStatus::Idle);
    assert!(session.external_session_id.is_none());
    assert!(session.codex_thread_state.is_none());
    assert!(session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn stopped by user."
    )));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(record.external_session_id.is_none());
    assert!(record.session.external_session_id.is_none());
    assert!(record.session.codex_thread_state.is_none());
    drop(inner);

    let reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    let reloaded = reloaded_inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should persist");
    assert!(reloaded.external_session_id.is_none());
    assert!(reloaded.session.external_session_id.is_none());
    assert!(reloaded.session.codex_thread_state.is_none());

    assert!(
        !runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned")
            .contains_key(&session_id)
    );
    assert!(
        !runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("thread-stop-fail")
    );

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that stop session dispatches queued prompt after shared Codex interrupt failure.
#[test]
fn stop_session_dispatches_queued_prompt_after_shared_codex_interrupt_failure() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = SharedCodexRuntime {
        runtime_id: "shared-codex-stop-fail-queued".to_owned(),
        input_tx,
        process: process.clone(),
        sessions: SharedCodexSessions::new(),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
    };

    {
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-stop-fail-queued".to_owned()),
                turn_id: Some("turn-stop-fail-queued".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("thread-stop-fail-queued".to_owned(), session_id.clone());

    let handle = CodexRuntimeHandle {
        runtime_id: runtime.runtime_id.clone(),
        input_tx: runtime.input_tx.clone(),
        process: process.clone(),
        shared_session: Some(SharedCodexSessionHandle {
            runtime: runtime.clone(),
            session_id: session_id.clone(),
        }),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(handle);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
        set_record_external_session_id(
            &mut inner.sessions[index],
            Some("thread-stop-fail-queued".to_owned()),
        );
        inner.sessions[index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::User,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-shared-stop-fail".to_owned(),
                    timestamp: stamp_now(),
                    text: "queued prompt after failed interrupt".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[index]);
    }

    let queued_session_id = session_id.clone();
    let command_thread = std::thread::spawn(move || {
        let interrupt = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Codex interrupt command should arrive");
        match interrupt {
            CodexRuntimeCommand::InterruptTurn {
                thread_id,
                turn_id,
                response_tx,
            } => {
                assert_eq!(thread_id, "thread-stop-fail-queued");
                assert_eq!(turn_id, "turn-stop-fail-queued");
                let _ = response_tx.send(Err("interrupt failed".to_owned()));
            }
            _ => panic!("expected Codex turn interrupt command"),
        }

        let prompt = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("queued Codex prompt should be dispatched");
        match prompt {
            CodexRuntimeCommand::Prompt {
                session_id,
                command,
            } => {
                assert_eq!(session_id, queued_session_id);
                assert_eq!(command.prompt, "queued prompt after failed interrupt");
                assert!(command.resume_thread_id.is_none());
            }
            _ => panic!("expected queued Codex prompt dispatch"),
        }
    });

    let snapshot = state.stop_session(&session_id).unwrap();
    command_thread
        .join()
        .expect("shared Codex command thread should join cleanly");

    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should remain present");
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(session.preview, "queued prompt after failed interrupt");
    assert!(session.external_session_id.is_none());
    assert!(session.codex_thread_state.is_none());
    assert!(session.pending_prompts.is_empty());
    assert!(session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn stopped by user."
    )));
    assert!(session.messages.iter().any(|message| matches!(
        message,
        Message::Text {
            author: Author::You,
            text,
            ..
        } if text == "queued prompt after failed interrupt"
    )));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert_eq!(record.session.status, SessionStatus::Active);
    assert!(matches!(record.runtime, SessionRuntime::Codex(_)));
    assert!(!record.runtime_stop_in_progress);
    assert!(record.queued_prompts.is_empty());
    assert!(record.external_session_id.is_none());
    assert!(record.session.external_session_id.is_none());
    assert!(record.session.codex_thread_state.is_none());
    drop(inner);

    let reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    let reloaded = reloaded_inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should persist");
    assert!(reloaded.external_session_id.is_none());
    assert!(reloaded.session.external_session_id.is_none());
    assert!(reloaded.session.codex_thread_state.is_none());
    assert!(
        !runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("thread-stop-fail-queued")
    );

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that stop session returns an error when a dedicated runtime refuses to stop.
#[test]
fn stop_session_returns_an_error_when_a_dedicated_runtime_refuses_to_stop() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-fail".to_owned(),
        input_tx,
        process: process.clone(),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::User,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-stop-follow-up".to_owned(),
                    timestamp: stamp_now(),
                    text: "queued prompt".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[index]);
    }

    let baseline_revision = state.snapshot().revision;
    let mut state_events = state.subscribe_events();
    let _failure_guard = force_test_kill_child_process_failure(&process, "Claude");
    let error = match state.stop_session(&session_id) {
        Ok(_) => panic!("failed dedicated runtime kills should not be treated as clean stops"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to stop session `"));
    assert_eq!(state.snapshot().revision, baseline_revision);
    assert!(matches!(
        state_events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.session.status, SessionStatus::Active);
    assert_eq!(record.session.preview, "Ready for a prompt.");
    assert!(matches!(record.runtime, SessionRuntime::Claude(_)));
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(record.session.pending_prompts.len(), 1);
    drop(inner);

    assert!(input_rx.recv_timeout(Duration::from_millis(100)).is_err());

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that stop session keeps the previous state visible until shutdown completes.
#[test]
fn stop_session_keeps_the_previous_state_visible_until_shutdown_completes() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-concurrent-read".to_owned(),
        input_tx,
        process: process.clone(),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
    }

    let stop_state = state.clone();
    let stop_session_id = session_id.clone();
    let stop_handle = std::thread::spawn(move || stop_state.stop_session(&stop_session_id));

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        {
            let inner = state.inner.lock().expect("state mutex poisoned");
            let record = inner
                .sessions
                .iter()
                .find(|record| record.session.id == session_id)
                .expect("Claude session should exist");
            if record.runtime_stop_in_progress {
                assert_eq!(record.session.status, SessionStatus::Active);
                assert_eq!(record.session.preview, "Streaming reply...");
                break;
            }
        }

        if std::time::Instant::now() >= deadline {
            panic!("stop_session did not enter the shutdown window in time");
        }

        std::thread::sleep(Duration::from_millis(5));
    }

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should still be visible while stopping");
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(session.preview, "Streaming reply...");

    let stopped_snapshot = stop_handle
        .join()
        .expect("stop_session thread should join cleanly")
        .expect("stop_session should succeed");
    let stopped_session = stopped_snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("stopped session should remain present");
    assert_eq!(stopped_session.status, SessionStatus::Idle);
    assert_eq!(stopped_session.preview, "Turn stopped by user.");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert!(!record.runtime_stop_in_progress);
    assert!(matches!(record.runtime, SessionRuntime::None));
    drop(inner);

    assert!(input_rx.recv_timeout(Duration::from_millis(100)).is_err());

    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that stop session returns conflict when already stopping.
#[test]
fn stop_session_returns_conflict_when_already_stopping() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-conflict".to_owned(),
        input_tx,
        process,
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
    }

    let stop_state = state.clone();
    let stop_session_id = session_id.clone();
    let stop_handle = std::thread::spawn(move || stop_state.stop_session(&stop_session_id));

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let stop_in_progress = {
            let inner = state.inner.lock().expect("state mutex poisoned");
            inner
                .sessions
                .iter()
                .find(|record| record.session.id == session_id)
                .expect("Claude session should exist")
                .runtime_stop_in_progress
        };
        if stop_in_progress {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!("stop_session did not enter the shutdown window in time");
        }

        std::thread::sleep(Duration::from_millis(5));
    }

    let error = match state.stop_session(&session_id) {
        Ok(_) => panic!("a second stop should conflict while shutdown is in flight"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, "session is already stopping");

    let stopped_snapshot = stop_handle
        .join()
        .expect("stop_session thread should join cleanly")
        .expect("initial stop_session should succeed");
    let stopped_session = stopped_snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("stopped session should remain present");
    assert_eq!(stopped_session.status, SessionStatus::Idle);
    assert_eq!(stopped_session.preview, "Turn stopped by user.");

    assert!(input_rx.recv_timeout(Duration::from_millis(100)).is_err());

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that runtime turn callbacks are suppressed while stop is in progress.
#[test]
fn runtime_turn_callbacks_are_suppressed_while_stop_is_in_progress() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, input_rx) = test_claude_runtime_handle("claude-stop-callback-guard");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
        inner.sessions[index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::User,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-stop-callback-guard".to_owned(),
                    timestamp: stamp_now(),
                    text: "queued prompt".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[index]);
        inner.sessions[index].runtime_stop_in_progress = true;
    }

    let baseline_revision = state.snapshot().revision;
    let mut state_events = state.subscribe_events();

    state
        .fail_turn_if_runtime_matches(&session_id, &runtime_token, "reader failure")
        .expect("fail_turn_if_runtime_matches should succeed");
    state
        .note_turn_retry_if_runtime_matches(&session_id, &runtime_token, "Retrying Claude...")
        .expect("note_turn_retry_if_runtime_matches should succeed");
    state
        .mark_turn_error_if_runtime_matches(&session_id, &runtime_token, "runtime error")
        .expect("mark_turn_error_if_runtime_matches should succeed");
    state
        .finish_turn_ok_if_runtime_matches(&session_id, &runtime_token)
        .expect("finish_turn_ok_if_runtime_matches should succeed");

    assert_eq!(state.snapshot().revision, baseline_revision);
    assert!(matches!(
        state_events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.session.status, SessionStatus::Active);
    assert_eq!(record.session.preview, "Streaming reply...");
    assert!(record.session.messages.is_empty());
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(record.session.pending_prompts.len(), 1);
    assert!(matches!(record.runtime, SessionRuntime::Claude(_)));
    assert!(record.runtime_stop_in_progress);
    assert_eq!(
        record.deferred_stop_callbacks,
        vec![
            DeferredStopCallback::TurnFailed("reader failure".to_owned()),
            DeferredStopCallback::TurnError("runtime error".to_owned()),
            DeferredStopCallback::TurnCompleted,
        ]
    );
    drop(inner);

    assert!(input_rx.recv_timeout(Duration::from_millis(100)).is_err());

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn fail_turn_if_runtime_matches_publishes_error_state_when_persist_fails() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, _input_rx) = test_claude_runtime_handle("claude-fail-turn-persist-fallback");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-fail-turn-persist-fallback-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
    }

    state.persistence_path = Arc::new(failing_persistence_path.clone());
    state.persist_tx = mpsc::channel().0;

    let baseline_revision = state.snapshot().revision;
    let mut state_events = state.subscribe_events();

    state
        .fail_turn_if_runtime_matches(&session_id, &runtime_token, "persist fallback failure")
        .expect("fail_turn_if_runtime_matches should publish even when persistence fails");

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(snapshot.revision, baseline_revision + 1);
    assert_eq!(session.status, SessionStatus::Error);
    assert_eq!(session.preview, "persist fallback failure");
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Turn failed: persist fallback failure"
    ));

    let published: StateResponse = serde_json::from_str(
        &state_events
            .try_recv()
            .expect("fail_turn_if_runtime_matches should publish a state snapshot"),
    )
    .expect("published state snapshot should decode");
    let published_session = published
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("published session should be present");
    assert_eq!(published.revision, snapshot.revision);
    assert_eq!(published_session.status, SessionStatus::Error);
    assert_eq!(published_session.preview, "persist fallback failure");
    assert!(matches!(
        published_session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Turn failed: persist fallback failure"
    ));

    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Tests that Codex thread state updates are suppressed while stop is in progress.
#[test]
fn codex_thread_state_updates_are_suppressed_while_stop_is_in_progress() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx) = test_codex_runtime_handle("codex-stop-thread-state-guard");
    let runtime_token = RuntimeToken::Codex(runtime.runtime_id.clone());
    state
        .set_external_session_id(&session_id, "thread-stop-guard".to_owned())
        .expect("Codex session should accept external thread ids");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
        inner.sessions[index].runtime_stop_in_progress = true;
    }

    let baseline_revision = state.snapshot().revision;
    let mut state_events = state.subscribe_events();

    state
        .set_codex_thread_state_if_runtime_matches(
            &session_id,
            &runtime_token,
            CodexThreadState::Archived,
        )
        .expect("set_codex_thread_state_if_runtime_matches should succeed");

    assert_eq!(state.snapshot().revision, baseline_revision);
    assert!(matches!(
        state_events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert_eq!(record.session.status, SessionStatus::Active);
    assert_eq!(record.session.preview, "Streaming reply...");
    assert_eq!(
        record.session.codex_thread_state,
        Some(CodexThreadState::Active)
    );
    assert!(matches!(record.runtime, SessionRuntime::Codex(_)));
    assert!(record.runtime_stop_in_progress);
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that shared Codex runtime exit clears state and kills the helper process.
#[test]
fn shared_codex_runtime_exit_clears_state_and_kills_process() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = SharedCodexRuntime {
        runtime_id: "shared-codex-timeout".to_owned(),
        input_tx,
        process: process.clone(),
        sessions: SharedCodexSessions::new(),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    let handle = CodexRuntimeHandle {
        runtime_id: runtime.runtime_id.clone(),
        input_tx: runtime.input_tx.clone(),
        process: process.clone(),
        shared_session: Some(SharedCodexSessionHandle {
            runtime: runtime.clone(),
            session_id: session_id.clone(),
        }),
    };

    {
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(handle);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
    }

    state
        .handle_shared_codex_runtime_exit(
            "shared-codex-timeout",
            Some("failed to communicate with shared Codex app-server"),
        )
        .expect("shared Codex runtime exit should succeed");

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Codex session should remain present");
    assert_eq!(session.status, SessionStatus::Error);
    assert!(
        session
            .preview
            .contains("failed to communicate with shared Codex app-server")
    );

    assert!(
        state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned")
            .is_none()
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert!(matches!(record.runtime, SessionRuntime::None));
    drop(inner);

    let _ = process.kill();
    let _ = wait_for_shared_child_exit_timeout(
        &process,
        Duration::from_secs(3),
        "shared Codex runtime",
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn shared_codex_stdin_watchdog_times_out_stalled_writer_and_clears_runtime() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = SharedCodexRuntime {
        runtime_id: "shared-codex-stdin-watchdog".to_owned(),
        input_tx,
        process: process.clone(),
        sessions: SharedCodexSessions::new(),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    let handle = CodexRuntimeHandle {
        runtime_id: runtime.runtime_id.clone(),
        input_tx: runtime.input_tx.clone(),
        process: process.clone(),
        shared_session: Some(SharedCodexSessionHandle {
            runtime: runtime.clone(),
            session_id: session_id.clone(),
        }),
    };

    {
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(handle);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
    }

    let activity: SharedCodexStdinActivityState =
        Arc::new(Mutex::new(Some(SharedCodexStdinActivity {
            operation: "flush",
            started_at: std::time::Instant::now() - Duration::from_millis(50),
            timed_out: false,
        })));
    let (_stop_tx, stop_rx) = mpsc::channel();
    spawn_shared_codex_stdin_watchdog(
        &state,
        &runtime.runtime_id,
        process.clone(),
        &activity,
        stop_rx,
        Duration::from_millis(10),
        Duration::from_millis(5),
    )
    .expect("shared Codex stdin watchdog should spawn");

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let cleared = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned")
            .is_none();
        if cleared {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "shared Codex stdin watchdog should tear down the stalled runtime"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Codex session should remain present");
    assert_eq!(session.status, SessionStatus::Error);
    assert!(
        session.preview.contains("Agent communication timed out"),
        "watchdog timeout should use generic message, got: {}",
        session.preview,
    );

    let _ = process.kill();
    let _ = wait_for_shared_child_exit_timeout(
        &process,
        Duration::from_secs(3),
        "shared Codex runtime",
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that runtime exit is suppressed while stop is in progress.
#[test]
fn runtime_exit_is_suppressed_while_stop_is_in_progress() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, _input_rx) = test_claude_runtime_handle("claude-stop-exit-guard");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
        inner.sessions[index].runtime_stop_in_progress = true;
    }

    let baseline_revision = state.snapshot().revision;
    let mut state_events = state.subscribe_events();

    state
        .handle_runtime_exit_if_matches(&session_id, &runtime_token, Some("runtime exited"))
        .expect("handle_runtime_exit_if_matches should succeed");

    assert_eq!(state.snapshot().revision, baseline_revision);
    assert!(matches!(
        state_events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.session.status, SessionStatus::Active);
    assert_eq!(record.session.preview, "Streaming reply...");
    assert!(record.session.messages.is_empty());
    assert!(matches!(record.runtime, SessionRuntime::Claude(_)));
    assert!(record.runtime_stop_in_progress);
    assert_eq!(
        record.deferred_stop_callbacks,
        vec![DeferredStopCallback::RuntimeExited(Some(
            "runtime exited".to_owned()
        ))]
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that successful stop discards deferred callbacks.
#[test]
fn successful_stop_discards_deferred_callbacks() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-discard-deferred".to_owned(),
        input_tx,
        process: process.clone(),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
        inner.sessions[index].deferred_stop_callbacks = vec![DeferredStopCallback::TurnCompleted];
    }

    let snapshot = state.stop_session(&session_id).unwrap();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("stopped session should remain present");
    assert_eq!(session.status, SessionStatus::Idle);
    assert_eq!(session.preview, "Turn stopped by user.");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert_eq!(record.session.preview, "Turn stopped by user.");
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.runtime_stop_in_progress);
    assert!(record.deferred_stop_callbacks.is_empty());
    drop(inner);

    assert!(input_rx.recv_timeout(Duration::from_millis(100)).is_err());
    process.wait().unwrap();

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that failed dedicated stop replays deferred turn completion.
#[test]
fn failed_dedicated_stop_replays_deferred_turn_completion() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-replay".to_owned(),
        input_tx,
        process: process.clone(),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
    }

    // Pre-stage a deferred completion callback. In production this would be stored by
    // `finish_turn_ok_if_runtime_matches` arriving during the shutdown window; here we set it
    // directly because the forced kill failure completes synchronously with no observable window.
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].deferred_stop_callbacks = vec![DeferredStopCallback::TurnCompleted];
    }

    let _failure_guard = force_test_kill_child_process_failure(&process, "Claude");
    let error = match state.stop_session(&session_id) {
        Ok(_) => panic!("failed dedicated runtime kills should not be treated as clean stops"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to stop session `"));

    // The deferred callback should have been replayed: session should now be Idle with the
    // runtime detached, just as if `finish_turn_ok_if_runtime_matches` had run normally.
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert!(!record.runtime_stop_in_progress);
    assert!(record.deferred_stop_callbacks.is_empty());
    drop(inner);

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that failed dedicated stop replays deferred runtime exit.
#[test]
fn failed_dedicated_stop_replays_deferred_runtime_exit() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-exit-replay".to_owned(),
        input_tx,
        process: process.clone(),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
    }

    // Pre-stage a deferred exit callback with an error message.
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].deferred_stop_callbacks = vec![DeferredStopCallback::RuntimeExited(
            Some("process crashed".to_owned()),
        )];
    }

    let _failure_guard = force_test_kill_child_process_failure(&process, "Claude");
    let error = match state.stop_session(&session_id) {
        Ok(_) => panic!("failed dedicated runtime kills should not be treated as clean stops"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);

    // The replayed exit callback should have transitioned the session to Error.
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.session.status, SessionStatus::Error);
    assert!(record.session.preview.contains("process crashed"));
    assert!(!record.runtime_stop_in_progress);
    assert!(record.deferred_stop_callbacks.is_empty());
    assert!(matches!(record.runtime, SessionRuntime::None));
    drop(inner);

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that failed dedicated stop replays multiple deferred callbacks in order.
#[test]
fn failed_dedicated_stop_replays_multiple_deferred_callbacks_in_order() {
    let expected_state = test_app_state();
    let expected_session_id = test_session_id(&expected_state, Agent::Claude);
    let expected_process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (expected_input_tx, _expected_input_rx) = mpsc::channel();
    let expected_runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-replay-order-expected".to_owned(),
        input_tx: expected_input_tx,
        process: expected_process.clone(),
    };
    let expected_token = RuntimeToken::Claude(expected_runtime.runtime_id.clone());

    let initial_message_count = {
        let mut inner = expected_state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&expected_session_id)
            .expect("Claude session should exist");
        let message_id = inner.next_message_id();
        inner.sessions[index].runtime = SessionRuntime::Claude(expected_runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Partial reply".to_owned();
        inner.sessions[index].session.messages.push(Message::Text {
            attachments: Vec::new(),
            id: message_id,
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: "Partial reply".to_owned(),
            expanded_text: None,
        });
        inner.sessions[index].session.messages.len()
    };

    expected_state
        .finish_turn_ok_if_runtime_matches(&expected_session_id, &expected_token)
        .expect("finish_turn_ok_if_runtime_matches should succeed");
    expected_state
        .handle_runtime_exit_if_matches(&expected_session_id, &expected_token, None)
        .expect("handle_runtime_exit_if_matches should succeed");

    let (expected_status, expected_preview, expected_message_count, expected_message_text) = {
        let inner = expected_state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == expected_session_id)
            .expect("Claude session should exist");
        let expected_message_text = match record.session.messages.last() {
            Some(Message::Text { text, .. }) => text.clone(),
            _ => panic!("Claude session should end with a text message"),
        };
        (
            record.session.status,
            record.session.preview.clone(),
            record.session.messages.len(),
            expected_message_text,
        )
    };
    assert_eq!(expected_status, SessionStatus::Idle);
    assert_eq!(expected_preview, "Partial reply");
    assert_eq!(expected_message_count, initial_message_count);
    assert_eq!(expected_message_text, "Partial reply");

    expected_process.kill().unwrap();
    expected_process.wait().unwrap();
    let _ = fs::remove_file(expected_state.persistence_path.as_path());

    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-replay-order".to_owned(),
        input_tx,
        process: process.clone(),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        let message_id = inner.next_message_id();
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Partial reply".to_owned();
        inner.sessions[index].session.messages.push(Message::Text {
            attachments: Vec::new(),
            id: message_id,
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: "Partial reply".to_owned(),
            expanded_text: None,
        });
        inner.sessions[index].deferred_stop_callbacks = vec![
            DeferredStopCallback::TurnCompleted,
            DeferredStopCallback::RuntimeExited(None),
        ];
    }

    let _failure_guard = force_test_kill_child_process_failure(&process, "Claude");
    let error = match state.stop_session(&session_id) {
        Ok(_) => panic!("failed dedicated runtime kills should not be treated as clean stops"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.session.status, expected_status);
    assert_eq!(record.session.preview, expected_preview);
    assert_eq!(record.session.messages.len(), expected_message_count);
    assert!(matches!(
        record.session.messages.last(),
        Some(Message::Text { text, .. }) if text == &expected_message_text
    ));
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.runtime_stop_in_progress);
    assert!(record.deferred_stop_callbacks.is_empty());
    drop(inner);

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that failed dedicated stop replays runtime exit last even when it arrives first.
#[test]
fn failed_dedicated_stop_replays_runtime_exit_last_even_when_it_arrives_first() {
    let expected_state = test_app_state();
    let expected_session_id = test_session_id(&expected_state, Agent::Claude);
    let expected_process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (expected_input_tx, _expected_input_rx) = mpsc::channel();
    let expected_runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-replay-reversed-expected".to_owned(),
        input_tx: expected_input_tx,
        process: expected_process.clone(),
    };
    let expected_token = RuntimeToken::Claude(expected_runtime.runtime_id.clone());

    let initial_message_count = {
        let mut inner = expected_state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&expected_session_id)
            .expect("Claude session should exist");
        let message_id = inner.next_message_id();
        inner.sessions[index].runtime = SessionRuntime::Claude(expected_runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Partial reply".to_owned();
        inner.sessions[index].session.messages.push(Message::Text {
            attachments: Vec::new(),
            id: message_id,
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: "Partial reply".to_owned(),
            expanded_text: None,
        });
        inner.sessions[index].session.messages.len()
    };

    expected_state
        .finish_turn_ok_if_runtime_matches(&expected_session_id, &expected_token)
        .expect("finish_turn_ok_if_runtime_matches should succeed");
    expected_state
        .handle_runtime_exit_if_matches(&expected_session_id, &expected_token, None)
        .expect("handle_runtime_exit_if_matches should succeed");

    let (expected_status, expected_preview, expected_message_count, expected_message_text) = {
        let inner = expected_state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == expected_session_id)
            .expect("Claude session should exist");
        let expected_message_text = match record.session.messages.last() {
            Some(Message::Text { text, .. }) => text.clone(),
            _ => panic!("Claude session should end with a text message"),
        };
        (
            record.session.status,
            record.session.preview.clone(),
            record.session.messages.len(),
            expected_message_text,
        )
    };
    assert_eq!(expected_status, SessionStatus::Idle);
    assert_eq!(expected_preview, "Partial reply");
    assert_eq!(expected_message_count, initial_message_count);
    assert_eq!(expected_message_text, "Partial reply");

    expected_process.kill().unwrap();
    expected_process.wait().unwrap();
    let _ = fs::remove_file(expected_state.persistence_path.as_path());

    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-replay-reversed".to_owned(),
        input_tx,
        process: process.clone(),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        let message_id = inner.next_message_id();
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Partial reply".to_owned();
        inner.sessions[index].session.messages.push(Message::Text {
            attachments: Vec::new(),
            id: message_id,
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: "Partial reply".to_owned(),
            expanded_text: None,
        });
        inner.sessions[index].deferred_stop_callbacks = vec![
            DeferredStopCallback::RuntimeExited(None),
            DeferredStopCallback::TurnCompleted,
        ];
    }

    let _failure_guard = force_test_kill_child_process_failure(&process, "Claude");
    let error = match state.stop_session(&session_id) {
        Ok(_) => panic!("failed dedicated runtime kills should not be treated as clean stops"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.session.status, expected_status);
    assert_eq!(record.session.preview, expected_preview);
    assert_eq!(record.session.messages.len(), expected_message_count);
    assert!(matches!(
        record.session.messages.last(),
        Some(Message::Text { text, .. }) if text == &expected_message_text
    ));
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.runtime_stop_in_progress);
    assert!(record.deferred_stop_callbacks.is_empty());
    drop(inner);

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that syncs cursor model options from ACP config.
#[test]
fn syncs_cursor_model_options_from_acp_config() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor ACP".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Agent),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let model_options = vec![
        SessionModelOption::plain("Auto", "auto"),
        SessionModelOption::plain("GPT-5.3 Codex", "gpt-5.3-codex"),
    ];
    state
        .sync_session_model_options(
            &created.session_id,
            Some("gpt-5.3-codex".to_owned()),
            model_options.clone(),
        )
        .unwrap();

    let inner = state.inner.lock().expect("state mutex poisoned");
    let session = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .map(|record| &record.session)
        .expect("Cursor session should exist");
    assert_eq!(session.model, "gpt-5.3-codex");
    assert_eq!(session.model_options, model_options);
}

// Tests that cursor agent mode auto approves ACP permission requests.
#[test]
fn cursor_agent_mode_auto_approves_acp_permission_requests() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Agent".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Agent),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let (input_tx, input_rx) = mpsc::channel();
    let mut recorder = SessionRecorder::new(state.clone(), created.session_id.clone());

    handle_acp_request(
        &cursor_permission_request("cursor-agent-approval"),
        &state,
        &created.session_id,
        &input_tx,
        &mut recorder,
        AcpAgent::Cursor,
    )
    .unwrap();

    match input_rx
        .recv_timeout(Duration::from_millis(100))
        .expect("Cursor agent mode should auto-respond")
    {
        AcpRuntimeCommand::JsonRpcMessage(message) => {
            assert_eq!(message.get("id"), Some(&json!("cursor-agent-approval")));
            assert_eq!(
                message.pointer("/result/outcome/optionId"),
                Some(&json!("allow-once"))
            );
        }
        _ => panic!("expected automatic Cursor approval response"),
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Cursor session should exist");
    assert!(record.pending_acp_approvals.is_empty());
    assert!(record.session.messages.is_empty());
    assert_eq!(record.session.status, SessionStatus::Idle);
}

// Tests that cursor ask mode queues ACP permission requests.
#[test]
fn cursor_ask_mode_queues_acp_permission_requests() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Ask".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Ask),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let (input_tx, input_rx) = mpsc::channel();
    let mut recorder = SessionRecorder::new(state.clone(), created.session_id.clone());

    handle_acp_request(
        &cursor_permission_request("cursor-ask-approval"),
        &state,
        &created.session_id,
        &input_tx,
        &mut recorder,
        AcpAgent::Cursor,
    )
    .unwrap();

    assert!(matches!(
        input_rx.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Cursor session should exist");
    assert_eq!(record.pending_acp_approvals.len(), 1);
    assert!(matches!(
        record.session.messages.first(),
        Some(Message::Approval {
            title,
            command,
            decision,
            ..
        }) if title == "Cursor needs approval"
            && command == "Edit src/main.rs"
            && *decision == ApprovalDecision::Pending
    ));
    assert_eq!(record.session.status, SessionStatus::Approval);
}

// Tests that cursor plan mode rejects ACP permission requests.
#[test]
fn cursor_plan_mode_rejects_acp_permission_requests() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Plan".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Plan),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let (input_tx, input_rx) = mpsc::channel();
    let mut recorder = SessionRecorder::new(state.clone(), created.session_id.clone());

    handle_acp_request(
        &cursor_permission_request("cursor-plan-approval"),
        &state,
        &created.session_id,
        &input_tx,
        &mut recorder,
        AcpAgent::Cursor,
    )
    .unwrap();

    match input_rx
        .recv_timeout(Duration::from_millis(100))
        .expect("Cursor plan mode should auto-reject")
    {
        AcpRuntimeCommand::JsonRpcMessage(message) => {
            assert_eq!(message.get("id"), Some(&json!("cursor-plan-approval")));
            assert_eq!(
                message.pointer("/result/outcome/optionId"),
                Some(&json!("reject-once"))
            );
        }
        _ => panic!("expected automatic Cursor rejection response"),
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Cursor session should exist");
    assert!(record.pending_acp_approvals.is_empty());
    assert!(record.session.messages.is_empty());
    assert_eq!(record.session.status, SessionStatus::Idle);
}

// Tests that syncs cursor mode from ACP config updates.
#[test]
fn syncs_cursor_mode_from_acp_config_updates() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Config Sync".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Agent),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let mut recorder = SessionRecorder::new(state.clone(), created.session_id.clone());
    let mut turn_state = AcpTurnState::default();

    handle_acp_session_update(
        &json!({
            "sessionUpdate": "config_update",
            "configOptions": [
                {
                    "id": "model",
                    "currentValue": "auto",
                    "options": [{ "value": "auto", "name": "Auto" }]
                },
                {
                    "id": "mode",
                    "currentValue": "ask",
                    "options": [
                        { "value": "agent" },
                        { "value": "ask" },
                        { "value": "plan" }
                    ]
                }
            ]
        }),
        &state,
        &created.session_id,
        &mut turn_state,
        &mut recorder,
        AcpAgent::Cursor,
    )
    .unwrap();

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Cursor session should exist");
    assert_eq!(record.session.cursor_mode, Some(CursorMode::Ask));
}

// Tests that syncs cursor mode from mode updates.
#[test]
fn syncs_cursor_mode_from_mode_updates() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Mode Sync".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Ask),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let mut recorder = SessionRecorder::new(state.clone(), created.session_id.clone());
    let mut turn_state = AcpTurnState::default();

    handle_acp_session_update(
        &json!({
            "sessionUpdate": "mode_update",
            "mode": "plan"
        }),
        &state,
        &created.session_id,
        &mut turn_state,
        &mut recorder,
        AcpAgent::Cursor,
    )
    .unwrap();

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Cursor session should exist");
    assert_eq!(record.session.cursor_mode, Some(CursorMode::Plan));
}

// Tests that borrowed session recorder uses shared message and request logic.
#[test]
fn borrowed_session_recorder_uses_shared_message_and_request_logic() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let questions = vec![UserInputQuestion {
        header: "Scope".to_owned(),
        id: "scope".to_owned(),
        is_other: false,
        is_secret: false,
        options: None,
        question: "What should Codex review?".to_owned(),
    }];
    let mut recorder_state = SessionRecorderState::default();
    let mut recorder = BorrowedSessionRecorder::new(&state, &session_id, &mut recorder_state);

    recorder.push_text("Initial text").unwrap();
    recorder.text_delta("streamed text").unwrap();
    recorder.finish_streaming_text().unwrap();
    recorder.command_started("cmd-1", "pwd").unwrap();
    recorder
        .command_completed("cmd-1", "pwd", "/tmp", CommandStatus::Success)
        .unwrap();
    recorder
        .push_codex_user_input_request(
            "Need input",
            "Choose the review scope.",
            questions.clone(),
            CodexPendingUserInput {
                questions: questions.clone(),
                request_id: json!("request-1"),
            },
        )
        .unwrap();

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");

    assert!(record.session.messages.iter().any(|message| {
        matches!(message, Message::Text { text, .. } if text == "Initial text")
    }));
    assert!(record.session.messages.iter().any(|message| {
        matches!(message, Message::Text { text, .. } if text == "streamed text")
    }));
    assert!(record.session.messages.iter().any(|message| {
        matches!(
            message,
            Message::Command {
                command,
                output,
                status,
                ..
            } if command == "pwd" && output == "/tmp" && *status == CommandStatus::Success
        )
    }));
    assert!(record.session.messages.iter().any(|message| {
        matches!(
            message,
            Message::UserInputRequest {
                title,
                detail,
                questions: message_questions,
                state,
                ..
            } if title == "Need input"
                && detail == "Choose the review scope."
                && message_questions == &questions
                && *state == InteractionRequestState::Pending
        )
    }));
    assert_eq!(record.pending_codex_user_inputs.len(), 1);
}

// Tests that updates live cursor mode on active ACP sessions.
#[test]
fn updates_live_cursor_mode_on_active_acp_sessions() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Live Mode".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Agent),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let (runtime, input_rx) = test_acp_runtime_handle(AcpAgent::Cursor, "cursor-live-mode");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("Cursor session should exist");
        inner.sessions[index].runtime = SessionRuntime::Acp(runtime);
        inner.sessions[index].external_session_id = Some("cursor-session-1".to_owned());
    }

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: None,
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: Some(CursorMode::Ask),
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Cursor session should be present");
    assert_eq!(session.cursor_mode, Some(CursorMode::Ask));

    match input_rx
        .recv_timeout(Duration::from_millis(100))
        .expect("Cursor mode change should be forwarded to the live ACP session")
    {
        AcpRuntimeCommand::JsonRpcMessage(message) => {
            assert_eq!(
                message.get("method").and_then(Value::as_str),
                Some("session/set_config_option")
            );
            assert_eq!(
                message.pointer("/params/sessionId"),
                Some(&json!("cursor-session-1"))
            );
            assert_eq!(message.pointer("/params/optionId"), Some(&json!("mode")));
            assert_eq!(message.pointer("/params/value"), Some(&json!("ask")));
        }
        _ => panic!("expected live Cursor mode update request"),
    }
}

// Tests that matches ACP model options by name or label.
#[test]
fn matches_acp_model_options_by_name_or_label() {
    let config = json!({
        "configOptions": [
            {
                "id": "model",
                "options": [
                    {
                        "value": "auto",
                        "name": "Auto"
                    },
                    {
                        "value": "gpt-5.3-codex-high-fast",
                        "label": "GPT-5.3 Codex High Fast"
                    }
                ]
            }
        ]
    });

    assert_eq!(
        matching_acp_config_option_value(&config, "model", "Auto"),
        Some("auto".to_owned())
    );
    assert_eq!(
        matching_acp_config_option_value(&config, "model", "GPT-5.3 Codex High Fast"),
        Some("gpt-5.3-codex-high-fast".to_owned())
    );
    assert_eq!(
        matching_acp_config_option_value(&config, "model", "Missing Model"),
        None
    );
}

// Tests that canonicalizes session model updates from live model labels.
#[test]
fn canonicalizes_session_model_updates_from_live_model_labels() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Canonical".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5.4".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    state
        .sync_session_model_options(
            &created.session_id,
            None,
            vec![SessionModelOption::plain("GPT-5.4", "gpt-5.4")],
        )
        .unwrap();

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: Some("GPT-5.4".to_owned()),
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Codex session should be present");
    assert_eq!(session.model, "gpt-5.4");
}

// Tests that revisions increase for visible state changes.
#[test]
fn revisions_increase_for_visible_state_changes() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Revision Test".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    assert_eq!(created.state.revision, 1);

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: None,
                sandbox_mode: Some(CodexSandboxMode::ReadOnly),
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
        )
        .unwrap();
    assert_eq!(updated.revision, 2);

    let renamed = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: Some("Revision Test Renamed".to_owned()),
                model: None,
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
        )
        .unwrap();
    assert_eq!(renamed.revision, 3);
}

// Tests that renames sessions via settings updates.
#[test]
fn renames_sessions_via_settings_updates() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Old Name".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: Some("New Name".to_owned()),
                model: None,
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
        )
        .unwrap();

    let renamed = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("renamed session should be present");
    assert_eq!(renamed.name, "New Name");
}

// Tests that persists remote settings.
#[test]
fn persists_remote_settings() {
    let state = test_app_state();

    let updated = state
        .update_app_settings(UpdateAppSettingsRequest {
            default_codex_reasoning_effort: None,
            default_claude_approval_mode: None,
            default_claude_effort: None,
            remotes: Some(vec![
                RemoteConfig::local(),
                RemoteConfig {
                    id: "ssh-lab".to_owned(),
                    name: "SSH Lab".to_owned(),
                    transport: RemoteTransport::Ssh,
                    enabled: true,
                    host: Some("example.com".to_owned()),
                    port: Some(2222),
                    user: Some("alice".to_owned()),
                },
            ]),
        })
        .unwrap();

    assert_eq!(updated.preferences.remotes.len(), 2);
    assert_eq!(updated.preferences.remotes[1].id, "ssh-lab");
    assert_eq!(
        updated.preferences.remotes[1].transport,
        RemoteTransport::Ssh
    );

    let reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    assert_eq!(
        reloaded_inner.preferences.remotes,
        updated.preferences.remotes
    );
}

// Tests that rejects remote settings with unsafe remote ID.
#[test]
fn rejects_remote_settings_with_unsafe_remote_id() {
    let state = test_app_state();

    let error = match state.update_app_settings(UpdateAppSettingsRequest {
        default_codex_reasoning_effort: None,
        default_claude_approval_mode: None,
        default_claude_effort: None,
        remotes: Some(vec![
            RemoteConfig::local(),
            RemoteConfig {
                id: "ssh/lab".to_owned(),
                name: "SSH Lab".to_owned(),
                transport: RemoteTransport::Ssh,
                enabled: true,
                host: Some("example.com".to_owned()),
                port: Some(22),
                user: Some("alice".to_owned()),
            },
        ]),
    }) {
        Ok(_) => panic!("unsafe remote id should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error.message,
        "remote id `ssh/lab` contains unsupported characters"
    );
}

// Tests that rejects remote settings with invalid SSH host.
#[test]
fn rejects_remote_settings_with_invalid_ssh_host() {
    let state = test_app_state();

    let error = match state.update_app_settings(UpdateAppSettingsRequest {
        default_codex_reasoning_effort: None,
        default_claude_approval_mode: None,
        default_claude_effort: None,
        remotes: Some(vec![
            RemoteConfig::local(),
            RemoteConfig {
                id: "ssh-lab".to_owned(),
                name: "SSH Lab".to_owned(),
                transport: RemoteTransport::Ssh,
                enabled: true,
                host: Some("-oProxyCommand=touch/tmp/pwned".to_owned()),
                port: Some(22),
                user: Some("alice".to_owned()),
            },
        ]),
    }) {
        Ok(_) => panic!("host injection should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, "remote `SSH Lab` has an invalid SSH host");
}

// Tests that rejects remote settings with invalid SSH user.
#[test]
fn rejects_remote_settings_with_invalid_ssh_user() {
    let state = test_app_state();

    let error = match state.update_app_settings(UpdateAppSettingsRequest {
        default_codex_reasoning_effort: None,
        default_claude_approval_mode: None,
        default_claude_effort: None,
        remotes: Some(vec![
            RemoteConfig::local(),
            RemoteConfig {
                id: "ssh-lab".to_owned(),
                name: "SSH Lab".to_owned(),
                transport: RemoteTransport::Ssh,
                enabled: true,
                host: Some("example.com".to_owned()),
                port: Some(22),
                user: Some("alice@example.com".to_owned()),
            },
        ]),
    }) {
        Ok(_) => panic!("invalid SSH user should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, "remote `SSH Lab` has an invalid SSH user");
}

// Tests that remote connection issue message hides transport details.
#[test]
fn remote_connection_issue_message_hides_transport_details() {
    assert_eq!(
        remote_connection_issue_message("SSH Lab"),
        "Could not connect to remote \"SSH Lab\" over SSH. Check the host, network, and SSH settings, then try again."
    );
}

// Tests that local SSH start issue message hides transport details.
#[test]
fn local_ssh_start_issue_message_hides_transport_details() {
    assert_eq!(
        local_ssh_start_issue_message("SSH Lab"),
        "Could not start the local SSH client for remote \"SSH Lab\". Verify OpenSSH is installed and available on PATH, then try again."
    );
}

// Tests that remote SSH command args insert double dash before target.
#[test]
fn remote_ssh_command_args_insert_double_dash_before_target() {
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(2222),
        user: Some("alice".to_owned()),
    };

    let args = remote_ssh_command_args(&remote, 47001, RemoteProcessMode::ManagedServer)
        .expect("SSH args should build");

    let separator_index = args
        .iter()
        .position(|arg| arg == "--")
        .expect("SSH args should include `--` before the target");
    assert_eq!(args[separator_index + 1], "alice@example.com");
    assert_eq!(&args[separator_index + 2..], ["termal", "server"]);
}

// Tests that removing remote stops event bridge worker and resets started guard.
#[test]
fn removing_remote_stops_event_bridge_worker_and_resets_started_guard() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let connection = Arc::new(RemoteConnection {
        config: Mutex::new(remote.clone()),
        forwarded_port: 47001,
        process: Mutex::new(None),
        event_bridge_started: AtomicBool::new(false),
        event_bridge_shutdown: AtomicBool::new(false),
        supports_inline_orchestrator_templates: Mutex::new(None),
    });
    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(remote.id.clone(), connection.clone());

    connection.start_event_bridge(state.remote_registry.client.client().clone(), state.clone());
    assert!(connection.event_bridge_started.load(Ordering::SeqCst));

    state.remote_registry.reconcile(&[RemoteConfig::local()]);

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        if !connection.event_bridge_started.load(Ordering::SeqCst) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "event bridge worker should stop after the remote is removed"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    assert!(
        !state
            .remote_registry
            .connections
            .lock()
            .expect("remote registry mutex poisoned")
            .contains_key(&remote.id)
    );

    connection.start_event_bridge(state.remote_registry.client.client().clone(), state.clone());
    assert!(connection.event_bridge_started.load(Ordering::SeqCst));

    connection.stop_event_bridge();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        if !connection.event_bridge_started.load(Ordering::SeqCst) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "event bridge started guard should reset after shutdown"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

// Tests that event-bridge retry boundaries clear fallback resync tracking so a
// restarted remote can recover even if its revision counter drops.
#[test]
fn remote_event_bridge_retry_clears_fallback_resync_tracking() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let connection = Arc::new(RemoteConnection {
        config: Mutex::new(remote.clone()),
        forwarded_port: 47001,
        process: Mutex::new(None),
        event_bridge_started: AtomicBool::new(false),
        event_bridge_shutdown: AtomicBool::new(false),
        supports_inline_orchestrator_templates: Mutex::new(None),
    });
    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(remote.id.clone(), connection.clone());

    state.note_remote_sse_fallback_resync(&remote.id, 4);
    assert!(state.should_skip_remote_sse_fallback_resync(&remote.id, 4));

    connection.start_event_bridge(state.remote_registry.client.client().clone(), state.clone());

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        if !state.should_skip_remote_sse_fallback_resync(&remote.id, 4) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "event bridge retry should clear stale fallback tracking"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    connection.stop_event_bridge();

    let shutdown_deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        if !connection.event_bridge_started.load(Ordering::SeqCst) {
            break;
        }
        assert!(
            std::time::Instant::now() < shutdown_deadline,
            "event bridge worker should stop after shutdown"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that event-bridge retry boundaries also clear stale applied remote
// revisions so restarted remotes can resume syncing below the old watermark.
#[test]
fn remote_event_bridge_retry_clears_applied_revision_tracking() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let connection = Arc::new(RemoteConnection {
        config: Mutex::new(remote.clone()),
        forwarded_port: 47001,
        process: Mutex::new(None),
        event_bridge_started: AtomicBool::new(false),
        event_bridge_shutdown: AtomicBool::new(false),
        supports_inline_orchestrator_templates: Mutex::new(None),
    });
    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(remote.id.clone(), connection.clone());

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.note_remote_applied_revision(&remote.id, 4);
        assert!(inner.should_skip_remote_applied_revision(&remote.id, 4));
    }

    connection.start_event_bridge(state.remote_registry.client.client().clone(), state.clone());

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let still_skipping = {
            let inner = state.inner.lock().expect("state mutex poisoned");
            inner.should_skip_remote_applied_revision(&remote.id, 4)
        };
        if !still_skipping {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "event bridge retry should clear stale applied revision tracking"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    connection.stop_event_bridge();

    let shutdown_deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        if !connection.event_bridge_started.load(Ordering::SeqCst) {
            break;
        }
        assert!(
            std::time::Instant::now() < shutdown_deadline,
            "event bridge worker should stop after shutdown"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote snapshot sync localizes orchestrators and creates missing proxy sessions.
#[test]
fn remote_snapshot_sync_localizes_orchestrators_and_creates_missing_proxy_sessions() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    state
        .apply_remote_state_snapshot(
            &remote.id,
            sample_remote_orchestrator_state(
                "remote-project-1",
                "/remote/repo",
                1,
                OrchestratorInstanceStatus::Running,
            ),
        )
        .expect("remote snapshot should apply");

    let snapshot = state.snapshot();
    let orchestrator = snapshot
        .orchestrators
        .iter()
        .find(|instance| {
            instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
        })
        .cloned()
        .expect("remote orchestrator should be mirrored");
    assert_ne!(orchestrator.id, "remote-orchestrator-1");
    assert_eq!(orchestrator.remote_id.as_deref(), Some(remote.id.as_str()));
    assert_eq!(orchestrator.project_id, local_project_id);
    assert_eq!(
        orchestrator.template_snapshot.project_id.as_deref(),
        Some(local_project_id.as_str())
    );
    assert_eq!(orchestrator.session_instances.len(), 3);
    assert_eq!(orchestrator.pending_transitions.len(), 1);

    let localized_session_ids = orchestrator
        .session_instances
        .iter()
        .map(|session| session.session_id.clone())
        .collect::<HashSet<_>>();
    assert!(localized_session_ids.contains(&orchestrator.pending_transitions[0].source_session_id));
    assert!(
        localized_session_ids.contains(&orchestrator.pending_transitions[0].destination_session_id)
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    for remote_session_id in ["remote-session-1", "remote-session-2", "remote-session-3"] {
        let index = inner
            .find_remote_session_index(&remote.id, remote_session_id)
            .expect("remote mirrored session should exist");
        assert_eq!(
            inner.sessions[index].session.project_id.as_deref(),
            Some(local_project_id.as_str())
        );
        assert!(localized_session_ids.contains(&inner.sessions[index].session.id));
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that mirrored remote orchestrators never enqueue local pending prompts during resume.
#[test]
fn remote_mirrored_orchestrators_do_not_enqueue_local_pending_prompts_on_resume() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    state
        .apply_remote_state_snapshot(
            &remote.id,
            sample_remote_orchestrator_state(
                "remote-project-1",
                "/remote/repo",
                1,
                OrchestratorInstanceStatus::Running,
            ),
        )
        .expect("remote snapshot should apply");

    let destination_local_session_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-2")
            .expect("remote mirrored destination session should exist");
        inner.sessions[index].session.id.clone()
    };
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&destination_local_session_id)
            .expect("destination session should exist");
        inner.sessions[index].session.status = SessionStatus::Active;
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .resume_pending_orchestrator_transitions()
        .expect("mirrored remote orchestrators should be ignored during local resume");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let destination = inner
        .sessions
        .iter()
        .find(|record| record.session.id == destination_local_session_id)
        .expect("destination session should still exist");
    assert_eq!(destination.session.status, SessionStatus::Active);
    assert!(destination.queued_prompts.is_empty());
    assert!(destination.session.pending_prompts.is_empty());
    assert!(matches!(destination.runtime, SessionRuntime::None));
    let orchestrator = inner
        .orchestrator_instances
        .iter()
        .find(|instance| {
            instance.remote_id.as_deref() == Some(remote.id.as_str())
                && instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
        })
        .expect("mirrored remote orchestrator should still exist");
    assert_eq!(orchestrator.pending_transitions.len(), 1);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote mirrored orchestrators are skipped by local pending-transition dispatch.
#[test]
fn remote_mirrored_orchestrators_skip_pending_transition_dispatch() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-remote-orchestrator-next-action-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let project_id = create_test_project(&state, &project_root, "Remote Next Action");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();

    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let instance_index = inner
        .orchestrator_instances
        .iter()
        .position(|instance| instance.id == orchestrator.id)
        .expect("orchestrator instance should exist");
    inner.orchestrator_instances[instance_index].pending_transitions = vec![PendingTransition {
        id: "pending-local-remote-1".to_owned(),
        transition_id: "planner-to-builder".to_owned(),
        source_session_id: planner_session_id,
        destination_session_id: builder_session_id.clone(),
        completion_revision: 7,
        rendered_prompt: "Use this plan and implement it.".to_owned(),
        created_at: "2026-04-03 12:00:00".to_owned(),
    }];
    assert!(matches!(
        next_pending_transition_action(&inner, &HashSet::new()),
        Some(PendingTransitionAction::Deliver {
            destination_session_id,
            ..
        }) if destination_session_id == builder_session_id
    ));
    inner.orchestrator_instances[instance_index].remote_id = Some("ssh-lab".to_owned());
    inner.orchestrator_instances[instance_index].remote_orchestrator_id =
        Some("remote-orchestrator-1".to_owned());
    assert!(
        next_pending_transition_action(&inner, &HashSet::new()).is_none(),
        "remote mirrored orchestrators should not enqueue local pending actions"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_file(state.orchestrator_templates_path.as_path());
    let _ = fs::remove_dir_all(project_root);
}

// Tests that remote mirrored orchestrators are skipped by deadlock detection.
#[test]
fn remote_mirrored_orchestrators_skip_deadlock_detection() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-remote-orchestrator-deadlock-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let project_id = create_test_project(&state, &project_root, "Remote Deadlock");
    let template = state
        .create_orchestrator_template(sample_deadlocked_orchestrator_template_draft())
        .expect("deadlock template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let source_a_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "source-a")
        .expect("source-a session should be mapped")
        .session_id
        .clone();
    let source_b_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "source-b")
        .expect("source-b session should be mapped")
        .session_id
        .clone();
    let consolidate_a_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "consolidate-a")
        .expect("consolidate-a session should be mapped")
        .session_id
        .clone();
    let consolidate_b_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "consolidate-b")
        .expect("consolidate-b session should be mapped")
        .session_id
        .clone();

    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let instance_index = inner
        .orchestrator_instances
        .iter()
        .position(|instance| instance.id == orchestrator.id)
        .expect("orchestrator instance should exist");
    {
        let instance = &mut inner.orchestrator_instances[instance_index];
        instance.remote_id = Some("ssh-lab".to_owned());
        instance.remote_orchestrator_id = Some("remote-orchestrator-1".to_owned());
        instance.pending_transitions = vec![
            PendingTransition {
                id: "pending-consolidate-a".to_owned(),
                transition_id: "source-a-to-consolidate-a".to_owned(),
                source_session_id: source_a_session_id,
                destination_session_id: consolidate_a_session_id.clone(),
                completion_revision: 3,
                rendered_prompt: "Source A input.".to_owned(),
                created_at: "2026-04-03 12:05:00".to_owned(),
            },
            PendingTransition {
                id: "pending-consolidate-b".to_owned(),
                transition_id: "source-b-to-consolidate-b".to_owned(),
                source_session_id: source_b_session_id,
                destination_session_id: consolidate_b_session_id.clone(),
                completion_revision: 4,
                rendered_prompt: "Source B input.".to_owned(),
                created_at: "2026-04-03 12:06:00".to_owned(),
            },
        ];
    }

    let deadlocked_session_ids = detect_deadlocked_consolidate_session_ids(
        &inner,
        &inner.orchestrator_instances[instance_index],
    );
    assert_eq!(
        deadlocked_session_ids.into_iter().collect::<HashSet<_>>(),
        HashSet::from([
            consolidate_a_session_id.clone(),
            consolidate_b_session_id.clone(),
        ])
    );
    assert!(
        !mark_deadlocked_orchestrator_instances(&mut inner, &HashSet::new()),
        "remote mirrored orchestrators should not be marked as deadlocked"
    );
    let instance = &inner.orchestrator_instances[instance_index];
    assert_eq!(instance.status, OrchestratorInstanceStatus::Running);
    assert!(instance.error_message.is_none());
    assert_eq!(instance.pending_transitions.len(), 2);
    for session_id in [&consolidate_a_session_id, &consolidate_b_session_id] {
        let index = inner
            .find_session_index(session_id)
            .expect("consolidate session should exist");
        assert_eq!(inner.sessions[index].session.status, SessionStatus::Idle);
    }
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_file(state.orchestrator_templates_path.as_path());
    let _ = fs::remove_dir_all(project_root);
}

// Tests that remote OrchestratorsUpdated deltas localize ids and preserve proxy identity.
#[test]
fn remote_orchestrators_updated_delta_localizes_ids_and_preserves_proxy_identity() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    state
        .apply_remote_state_snapshot(
            &remote.id,
            sample_remote_orchestrator_state(
                "remote-project-1",
                "/remote/repo",
                1,
                OrchestratorInstanceStatus::Running,
            ),
        )
        .expect("initial remote snapshot should apply");

    let initial_snapshot = state.snapshot();
    let local_orchestrator_id = initial_snapshot
        .orchestrators
        .iter()
        .find(|instance| {
            instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
        })
        .expect("remote orchestrator should be mirrored")
        .id
        .clone();
    let mut delta_receiver = state.subscribe_delta_events();

    let remote_delta_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Paused,
    );
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::OrchestratorsUpdated {
                revision: 2,
                orchestrators: remote_delta_state.orchestrators.clone(),
                sessions: remote_delta_state.sessions.clone(),
            },
        )
        .expect("remote orchestrator delta should apply");

    let snapshot = state.snapshot();
    let orchestrator = snapshot
        .orchestrators
        .iter()
        .find(|instance| {
            instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
        })
        .expect("remote orchestrator should remain mirrored");
    assert_eq!(orchestrator.id, local_orchestrator_id);
    assert_eq!(orchestrator.status, OrchestratorInstanceStatus::Paused);
    assert_eq!(orchestrator.project_id, local_project_id);
    let expected_local_session_ids = orchestrator
        .session_instances
        .iter()
        .map(|instance| instance.session_id.clone())
        .collect::<HashSet<_>>();

    let delta_payload = delta_receiver
        .try_recv()
        .expect("localized orchestrator delta should be published");
    let delta: DeltaEvent =
        serde_json::from_str(&delta_payload).expect("delta payload should decode");
    match delta {
        DeltaEvent::OrchestratorsUpdated {
            revision,
            orchestrators,
            sessions,
        } => {
            assert_eq!(revision, snapshot.revision);
            let localized = orchestrators
                .iter()
                .find(|instance| {
                    instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
                })
                .expect("localized delta should contain the mirrored orchestrator");
            assert_eq!(localized.id, local_orchestrator_id);
            assert_eq!(localized.status, OrchestratorInstanceStatus::Paused);
            assert_eq!(
                sessions
                    .iter()
                    .map(|session| session.id.clone())
                    .collect::<HashSet<_>>(),
                expected_local_session_ids
            );
            assert!(sessions.iter().all(|session| {
                session.project_id.as_deref() == Some(local_project_id.as_str())
            }));
        }
        _ => panic!("unexpected delta variant"),
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote OrchestratorsUpdated deltas can create missing proxy sessions from their payload.
#[test]
fn remote_orchestrators_updated_delta_creates_missing_proxy_sessions_from_payload_sessions() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let remote_delta_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    );
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::OrchestratorsUpdated {
                revision: 1,
                orchestrators: remote_delta_state.orchestrators.clone(),
                sessions: remote_delta_state.sessions.clone(),
            },
        )
        .expect("remote orchestrator delta should create missing proxy sessions");

    let snapshot = state.snapshot();
    let orchestrator = snapshot
        .orchestrators
        .iter()
        .find(|instance| {
            instance.remote_id.as_deref() == Some(remote.id.as_str())
                && instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
        })
        .cloned()
        .expect("remote orchestrator should be mirrored from the delta payload");
    assert_eq!(orchestrator.project_id, local_project_id);
    assert_eq!(orchestrator.session_instances.len(), 3);

    let localized_session_ids = orchestrator
        .session_instances
        .iter()
        .map(|session| session.session_id.clone())
        .collect::<HashSet<_>>();
    let inner = state.inner.lock().expect("state mutex poisoned");
    for remote_session_id in ["remote-session-1", "remote-session-2", "remote-session-3"] {
        let index = inner
            .find_remote_session_index(&remote.id, remote_session_id)
            .expect("remote mirrored session should exist after delta localization");
        assert_eq!(
            inner.sessions[index].session.project_id.as_deref(),
            Some(local_project_id.as_str())
        );
        assert!(localized_session_ids.contains(&inner.sessions[index].session.id));
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that stale remote snapshots cannot overwrite newer orchestrator bridge deltas.
#[test]
fn stale_remote_snapshot_does_not_overwrite_newer_orchestrator_delta_state() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let remote_delta_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Paused,
    );
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::OrchestratorsUpdated {
                revision: 2,
                orchestrators: remote_delta_state.orchestrators.clone(),
                sessions: remote_delta_state.sessions.clone(),
            },
        )
        .expect("remote orchestrator delta should apply");
    let revision_after_delta = state.snapshot().revision;

    state
        .apply_remote_state_snapshot(
            &remote.id,
            sample_remote_orchestrator_state(
                "remote-project-1",
                "/remote/repo",
                1,
                OrchestratorInstanceStatus::Running,
            ),
        )
        .expect("stale remote snapshot should be ignored");

    let snapshot = state.snapshot();
    assert_eq!(snapshot.revision, revision_after_delta);
    let orchestrator = snapshot
        .orchestrators
        .iter()
        .find(|instance| {
            instance.remote_id.as_deref() == Some(remote.id.as_str())
                && instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
        })
        .expect("remote orchestrator should remain mirrored");
    assert_eq!(orchestrator.project_id, local_project_id);
    assert_eq!(orchestrator.status, OrchestratorInstanceStatus::Paused);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that failed remote OrchestratorsUpdated deltas roll back eager proxy-session localization.
#[test]
fn remote_orchestrators_updated_delta_rolls_back_proxy_sessions_when_localization_fails() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let mut delta_receiver = state.subscribe_delta_events();
    let (initial_session_count, initial_next_session_number) = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        (inner.sessions.len(), inner.next_session_number)
    };

    let mut invalid_delta_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    );
    invalid_delta_state
        .sessions
        .retain(|session| session.id != "remote-session-3");

    let error = state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::OrchestratorsUpdated {
                revision: 1,
                orchestrators: invalid_delta_state.orchestrators.clone(),
                sessions: invalid_delta_state.sessions.clone(),
            },
        )
        .expect_err("invalid remote orchestrator delta should fail localization");
    assert!(
        error
            .to_string()
            .contains("remote session `remote-session-3` not found"),
        "unexpected error: {error:#}"
    );
    assert!(
        delta_receiver.try_recv().is_err(),
        "failed remote delta should not publish a localized update"
    );

    let snapshot = state.snapshot();
    assert!(
        !snapshot
            .orchestrators
            .iter()
            .any(|instance| instance.remote_id.as_deref() == Some(remote.id.as_str()))
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.sessions.len(), initial_session_count);
    assert_eq!(inner.next_session_number, initial_next_session_number);
    assert!(
        inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .is_none()
    );
    assert!(
        inner
            .find_remote_session_index(&remote.id, "remote-session-2")
            .is_none()
    );
    assert!(
        inner
            .find_remote_session_index(&remote.id, "remote-session-3")
            .is_none()
    );
    drop(inner);

    let persisted: Value = serde_json::from_slice(
        &fs::read(state.persistence_path.as_path()).expect("persisted state file should exist"),
    )
    .expect("persisted state should deserialize");
    let persisted_sessions = persisted["sessions"]
        .as_array()
        .expect("persisted sessions should be present");
    assert_eq!(persisted_sessions.len(), initial_session_count);
    assert!(!persisted_sessions.iter().any(|candidate| {
        candidate["remoteSessionId"] == Value::String("remote-session-1".to_owned())
            || candidate["remoteSessionId"] == Value::String("remote-session-2".to_owned())
    }));
    let persisted_orchestrator_instances = persisted["orchestratorInstances"].as_array();
    assert!(persisted_orchestrator_instances.map_or(true, |instances| {
        !instances
            .iter()
            .any(|instance| instance["remoteId"] == Value::String(remote.id.clone()))
    }));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote deltas sharing a revision still apply sequentially.
#[test]
fn remote_same_revision_deltas_apply_in_sequence() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");

    let local_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let local_session_id = upsert_remote_proxy_session_record(
            &mut inner,
            &remote.id,
            &remote_session,
            Some(local_project_id),
        );
        state
            .commit_locked(&mut inner)
            .expect("remote proxy session should persist");
        local_session_id
    };
    let mut delta_receiver = state.subscribe_delta_events();

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::MessageCreated {
                revision: 2,
                session_id: "remote-session-1".to_owned(),
                message_id: "message-1".to_owned(),
                message_index: 0,
                message: Message::Text {
                    attachments: Vec::new(),
                    id: "message-1".to_owned(),
                    timestamp: "2026-04-05 10:00:00".to_owned(),
                    author: Author::Assistant,
                    text: "First remote message.".to_owned(),
                    expanded_text: None,
                },
                preview: "First remote message.".to_owned(),
                status: SessionStatus::Active,
            },
        )
        .expect("first same-revision delta should apply");
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::CommandUpdate {
                revision: 2,
                session_id: "remote-session-1".to_owned(),
                message_id: "command-1".to_owned(),
                message_index: 1,
                command: "echo ok".to_owned(),
                command_language: Some("bash".to_owned()),
                output: "ok".to_owned(),
                output_language: Some("text".to_owned()),
                status: CommandStatus::Success,
                preview: "echo ok".to_owned(),
            },
        )
        .expect("second same-revision delta should apply");

    let first_delta: DeltaEvent = serde_json::from_str(
        &delta_receiver
            .try_recv()
            .expect("first same-revision delta should publish"),
    )
    .expect("first delta payload should decode");
    match first_delta {
        DeltaEvent::MessageCreated { message_id, .. } => assert_eq!(message_id, "message-1"),
        _ => panic!("unexpected first delta variant"),
    }

    let second_delta: DeltaEvent = serde_json::from_str(
        &delta_receiver
            .try_recv()
            .expect("second same-revision delta should publish"),
    )
    .expect("second delta payload should decode");
    // A missing remote command message is localized by creating the message
    // first, so the published delta is normalized to MessageCreated.
    match second_delta {
        DeltaEvent::MessageCreated { message_id, .. } => assert_eq!(message_id, "command-1"),
        _ => panic!("unexpected second delta variant"),
    }

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == local_session_id)
        .expect("localized remote session should exist");
    assert_eq!(session.preview, "echo ok");
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(session.messages.len(), 2);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "First remote message."
    ));
    assert!(matches!(
        session.messages.get(1),
        Some(Message::Command {
            command,
            output,
            status: CommandStatus::Success,
            ..
        }) if command == "echo ok" && output == "ok"
    ));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote project orchestrator creation proxies to the remote backend and localizes the result.
#[test]
fn create_orchestrator_instance_proxies_remote_projects_and_localizes_response() {
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
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(local_project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;

    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    remote_state.orchestrators[0].id = "remote-orchestrator-created".to_owned();
    remote_state.orchestrators[0].template_id = template.id.clone();
    remote_state.orchestrators[0].template_snapshot = OrchestratorTemplate {
        id: template.id.clone(),
        name: template.name.clone(),
        description: template.description.clone(),
        project_id: Some("remote-project-1".to_owned()),
        sessions: template.sessions.clone(),
        transitions: template.transitions.clone(),
        created_at: template.created_at.clone(),
        updated_at: template.updated_at.clone(),
    };
    let remote_orchestrator = remote_state.orchestrators[0].clone();
    let remote_response = serde_json::to_string(&CreateOrchestratorInstanceResponse {
        orchestrator: remote_orchestrator,
        state: remote_state,
    })
    .expect("remote orchestrator response should encode");

    let captured = Arc::new(Mutex::new(None::<(String, String)>));
    let captured_for_server = captured.clone();
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = requests.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "test listener");
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

            let request_head = String::from_utf8_lossy(&buffer[..body_start]).to_string();
            let request_line = request_head
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();
            requests_for_server
                .lock()
                .expect("capture mutex poisoned")
                .push(request_line.clone());
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

            if request_line.starts_with("POST /api/orchestrators ") {
                *captured_for_server.lock().expect("capture mutex poisoned") =
                    Some((request_line.clone(), body));
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            remote_response.len(),
                            remote_response
                        )
                        .as_bytes(),
                    )
                    .expect("orchestrator response should write");
                continue;
            }

            panic!("unexpected request: {request_line}");
        }
    });

    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(true),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(None),
            }),
        );

    let response = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id.clone(),
            project_id: Some(local_project_id.clone()),
            template: None,
        })
        .expect("remote orchestrator should be created");

    assert_ne!(response.orchestrator.id, "remote-orchestrator-created");
    assert_eq!(
        response.orchestrator.remote_id.as_deref(),
        Some(remote.id.as_str())
    );
    assert_eq!(
        response.orchestrator.remote_orchestrator_id.as_deref(),
        Some("remote-orchestrator-created")
    );
    assert_eq!(response.orchestrator.project_id, local_project_id);
    assert_eq!(response.orchestrator.template_id, template.id);
    assert_eq!(
        response
            .orchestrator
            .template_snapshot
            .project_id
            .as_deref(),
        Some(response.orchestrator.project_id.as_str())
    );
    assert_eq!(
        response.orchestrator.session_instances.len(),
        template.sessions.len()
    );
    assert!(
        response
            .state
            .orchestrators
            .iter()
            .any(|instance| instance.id == response.orchestrator.id)
    );

    let (request_line, body) = captured
        .lock()
        .expect("capture mutex poisoned")
        .clone()
        .expect("captured request should exist");
    assert!(request_line.starts_with("POST /api/orchestrators "));
    let parsed_body: Value = serde_json::from_str(&body).expect("request body should decode");
    assert_eq!(
        parsed_body["templateId"],
        Value::String(template.id.clone())
    );
    assert_eq!(
        parsed_body["projectId"],
        Value::String("remote-project-1".to_owned())
    );
    assert_eq!(
        parsed_body["template"]["projectId"],
        Value::String("remote-project-1".to_owned())
    );
    assert_eq!(
        parsed_body["template"]["name"],
        Value::String(template.name)
    );

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that direct remote orchestrator proxy creation localizes the launch and notes the applied revision.
#[test]
fn create_remote_orchestrator_proxy_localizes_launch_and_notes_revision() {
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
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(local_project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;
    let project = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .find_project(&local_project_id)
            .cloned()
            .expect("remote project should exist")
    };

    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    remote_state.orchestrators[0].id = "remote-orchestrator-created".to_owned();
    remote_state.orchestrators[0].template_id = template.id.clone();
    remote_state.orchestrators[0].template_snapshot = OrchestratorTemplate {
        id: template.id.clone(),
        name: template.name.clone(),
        description: template.description.clone(),
        project_id: Some("remote-project-1".to_owned()),
        sessions: template.sessions.clone(),
        transitions: template.transitions.clone(),
        created_at: template.created_at.clone(),
        updated_at: template.updated_at.clone(),
    };
    let remote_response = serde_json::to_string(&CreateOrchestratorInstanceResponse {
        orchestrator: remote_state.orchestrators[0].clone(),
        state: remote_state,
    })
    .expect("remote orchestrator response should encode");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "test listener");
            let mut buffer = [0u8; 4096];
            let bytes_read = stream.read(&mut buffer).expect("request should read");
            assert!(bytes_read > 0, "request should contain data");
            let request = String::from_utf8_lossy(&buffer[..bytes_read]);
            let request_line = request
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
                    )
                    .expect("health response should write");
                continue;
            }

            if request_line.starts_with("POST /api/orchestrators ") {
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            remote_response.len(),
                            remote_response
                        )
                        .as_bytes(),
                    )
                    .expect("orchestrator response should write");
                continue;
            }

            panic!("unexpected request: {request_line}");
        }
    });

    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(true),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(None),
            }),
        );

    let response = state
        .create_remote_orchestrator_proxy(&template, &project)
        .expect("remote orchestrator should be localized");

    assert_eq!(
        response.orchestrator.remote_id.as_deref(),
        Some(remote.id.as_str())
    );
    assert_eq!(
        response.orchestrator.remote_orchestrator_id.as_deref(),
        Some("remote-orchestrator-created")
    );
    assert_eq!(response.orchestrator.project_id, local_project_id);
    assert!(
        response
            .state
            .orchestrators
            .iter()
            .any(|instance| instance.id == response.orchestrator.id)
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .find_remote_orchestrator_index(&remote.id, "remote-orchestrator-created")
            .is_some()
    );
    assert!(inner.should_skip_remote_applied_revision(&remote.id, 2));
    assert!(!inner.should_skip_remote_applied_revision(&remote.id, 3));
    drop(inner);

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that direct remote orchestrator proxy creation rolls back mirrored sessions and orchestrators when localization fails.
#[test]
fn create_remote_orchestrator_proxy_rolls_back_on_localization_failure() {
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
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(local_project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;
    let project = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .find_project(&local_project_id)
            .cloned()
            .expect("remote project should exist")
    };
    let persisted_before = fs::read(state.persistence_path.as_path())
        .expect("initial state should already be persisted");
    let initial_next_session_number = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner.next_session_number
    };

    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    remote_state.orchestrators[0].id = "remote-orchestrator-broken".to_owned();
    remote_state.orchestrators[0].template_id = template.id.clone();
    remote_state.orchestrators[0].template_snapshot = OrchestratorTemplate {
        id: template.id.clone(),
        name: template.name.clone(),
        description: template.description.clone(),
        project_id: Some("remote-project-1".to_owned()),
        sessions: template.sessions.clone(),
        transitions: template.transitions.clone(),
        created_at: template.created_at.clone(),
        updated_at: template.updated_at.clone(),
    };
    remote_state
        .sessions
        .retain(|session| session.id != "remote-session-1");
    let remote_response = serde_json::to_string(&CreateOrchestratorInstanceResponse {
        orchestrator: remote_state.orchestrators[0].clone(),
        state: remote_state,
    })
    .expect("remote orchestrator response should encode");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "test listener");
            let mut buffer = [0u8; 4096];
            let bytes_read = stream.read(&mut buffer).expect("request should read");
            assert!(bytes_read > 0, "request should contain data");
            let request = String::from_utf8_lossy(&buffer[..bytes_read]);
            let request_line = request
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
                    )
                    .expect("health response should write");
                continue;
            }

            if request_line.starts_with("POST /api/orchestrators ") {
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            remote_response.len(),
                            remote_response
                        )
                        .as_bytes(),
                    )
                    .expect("orchestrator response should write");
                continue;
            }

            panic!("unexpected request: {request_line}");
        }
    });

    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(true),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(None),
            }),
        );

    let err = match state.create_remote_orchestrator_proxy(&template, &project) {
        Ok(_) => panic!("invalid remote orchestrator should fail localization"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::BAD_GATEWAY);
    assert!(
        err.message
            .contains("remote orchestrator could not be localized")
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.next_session_number, initial_next_session_number);
    assert!(
        inner
            .find_remote_orchestrator_index(&remote.id, "remote-orchestrator-broken")
            .is_none()
    );
    assert!(
        inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .is_none()
    );
    assert!(
        inner
            .find_remote_session_index(&remote.id, "remote-session-2")
            .is_none()
    );
    assert!(!inner.should_skip_remote_applied_revision(&remote.id, 2));
    drop(inner);

    let persisted_after = fs::read(state.persistence_path.as_path())
        .expect("rolled back state should stay persisted");
    assert_eq!(persisted_after, persisted_before);

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that stale create responses still materialize the launched remote
// orchestrator when a newer unrelated revision has already been applied.
#[test]
fn create_orchestrator_instance_materializes_stale_remote_launch_response() {
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
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(local_project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;

    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    remote_state.orchestrators[0].id = "remote-orchestrator-created".to_owned();
    remote_state.orchestrators[0].template_id = template.id.clone();
    remote_state.orchestrators[0].template_snapshot = OrchestratorTemplate {
        id: template.id.clone(),
        name: template.name.clone(),
        description: template.description.clone(),
        project_id: Some("remote-project-1".to_owned()),
        sessions: template.sessions.clone(),
        transitions: template.transitions.clone(),
        created_at: template.created_at.clone(),
        updated_at: template.updated_at.clone(),
    };
    let remote_response = serde_json::to_string(&CreateOrchestratorInstanceResponse {
        orchestrator: remote_state.orchestrators[0].clone(),
        state: remote_state,
    })
    .expect("remote orchestrator response should encode");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "test listener");
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

            let request_head = String::from_utf8_lossy(&buffer[..body_start]).to_string();
            let request_line = request_head
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
                    )
                    .expect("health response should write");
                continue;
            }

            if request_line.starts_with("POST /api/orchestrators ") {
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            remote_response.len(),
                            remote_response
                        )
                        .as_bytes(),
                    )
                    .expect("orchestrator response should write");
                continue;
            }

            panic!("unexpected request: {request_line}");
        }
    });

    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(true),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(None),
            }),
        );
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.note_remote_applied_revision(&remote.id, 3);
    }

    let response = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id.clone(),
            project_id: Some(local_project_id.clone()),
            template: None,
        })
        .expect("stale launch response should still materialize the orchestrator");

    assert_eq!(
        response.orchestrator.remote_orchestrator_id.as_deref(),
        Some("remote-orchestrator-created")
    );
    assert_eq!(response.orchestrator.project_id, local_project_id);
    assert!(
        response
            .state
            .orchestrators
            .iter()
            .any(|instance| instance.id == response.orchestrator.id)
    );
    assert_eq!(
        response.orchestrator.session_instances.len(),
        template.sessions.len()
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .find_remote_orchestrator_index(&remote.id, "remote-orchestrator-created")
            .is_some()
    );
    assert!(inner.should_skip_remote_applied_revision(&remote.id, 3));
    assert!(!inner.should_skip_remote_applied_revision(&remote.id, 4));
    drop(inner);

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote orchestrator launch reports an upgrade requirement when the remote ignores inline templates.
#[test]
fn remote_orchestrator_create_requires_upgrade_when_remote_lacks_inline_template_support() {
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
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(local_project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;

    let captured = Arc::new(Mutex::new(None::<String>));
    let captured_for_server = captured.clone();
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = requests.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "test listener");
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

            let request_head = String::from_utf8_lossy(&buffer[..body_start]).to_string();
            let request_line = request_head
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();
            requests_for_server
                .lock()
                .expect("capture mutex poisoned")
                .push(request_line.clone());
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

            if request_line.starts_with("POST /api/orchestrators ") {
                *captured_for_server.lock().expect("capture mutex poisoned") = Some(body);
                let error_body =
                    "{\"error\":\"Inline template launch unavailable on this remote\"}";
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            error_body.len(),
                            error_body
                        )
                        .as_bytes(),
                    )
                    .expect("remote error response should write");
                continue;
            }

            panic!("unexpected request: {request_line}");
        }
    });

    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(true),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(None),
            }),
        );

    let err = match state.create_orchestrator_instance(CreateOrchestratorInstanceRequest {
        template_id: template.id.clone(),
        project_id: Some(local_project_id),
        template: None,
    }) {
        Ok(_) => panic!("old remote should require an upgrade for inline templates"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::BAD_GATEWAY);
    assert!(err.message.contains("must be upgraded"));
    // The cached capability should suppress only the post-404 diagnostic probe;
    // the normal pre-request availability probe still happens in ensure_available.
    assert_eq!(
        requests.lock().expect("capture mutex poisoned").clone(),
        vec![
            "GET /api/health HTTP/1.1".to_owned(),
            "POST /api/orchestrators HTTP/1.1".to_owned(),
        ]
    );
    let body = captured
        .lock()
        .expect("capture mutex poisoned")
        .clone()
        .expect("captured request should exist");
    let parsed_body: Value = serde_json::from_str(&body).expect("request body should decode");
    assert_eq!(
        parsed_body["templateId"],
        Value::String(template.id.clone())
    );
    assert_eq!(
        parsed_body["template"]["name"],
        Value::String(template.name.clone())
    );

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that a pre-cached unsupported inline-template capability still yields the upgrade message
// without issuing a second health probe after the remote returns 404. The initial
// ensure_available availability check is still expected before the launch attempt.
#[test]
fn remote_orchestrator_create_requires_upgrade_when_inline_template_support_is_precached_false() {
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
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(local_project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;

    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = requests.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "test listener");
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

            let request_head = String::from_utf8_lossy(&buffer[..body_start]).to_string();
            let request_line = request_head
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();
            requests_for_server
                .lock()
                .expect("capture mutex poisoned")
                .push(request_line.clone());

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
                    )
                    .expect("health response should write");
                continue;
            }

            if request_line.starts_with("POST /api/orchestrators ") {
                let error_body =
                    "{\"error\":\"Inline template launch unavailable on this remote\"}";
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            error_body.len(),
                            error_body
                        )
                        .as_bytes(),
                    )
                    .expect("remote error response should write");
                continue;
            }

            panic!("unexpected request: {request_line}");
        }
    });

    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(true),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(Some(false)),
            }),
        );

    assert_eq!(
        state
            .remote_registry
            .cached_supports_inline_orchestrator_templates(&remote),
        Some(false)
    );

    let err = match state.create_orchestrator_instance(CreateOrchestratorInstanceRequest {
        template_id: template.id.clone(),
        project_id: Some(local_project_id),
        template: None,
    }) {
        Ok(_) => panic!("old remote should require an upgrade for inline templates"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::BAD_GATEWAY);
    assert!(err.message.contains("must be upgraded"));
    // The cached Some(false) capability skips any extra post-404 health probe, but the
    // initial ensure_available probe still happens before the launch attempt.
    assert_eq!(
        requests.lock().expect("capture mutex poisoned").clone(),
        vec![
            "GET /api/health HTTP/1.1".to_owned(),
            "POST /api/orchestrators HTTP/1.1".to_owned(),
        ]
    );

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote state sync rolls back unmapped orchestrators instead of assigning an empty local project id.
#[test]
fn remote_snapshot_sync_skips_orchestrators_without_a_local_project_mapping() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.preferences.remotes.push(remote.clone());
        state
            .commit_locked(&mut inner)
            .expect("remote should persist");
    }

    state
        .apply_remote_state_snapshot(
            &remote.id,
            sample_remote_orchestrator_state(
                "remote-project-unmapped",
                "/remote/repo",
                1,
                OrchestratorInstanceStatus::Running,
            ),
        )
        .expect("snapshot should still apply even when orchestration localization fails");

    let snapshot = state.snapshot();
    assert!(snapshot.orchestrators.is_empty());
    assert!(snapshot.sessions.is_empty());

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.orchestrator_instances.is_empty());
    assert!(inner.sessions.is_empty());
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote orchestrator lifecycle actions proxy to the remote backend and resync local state.
#[test]
fn remote_orchestrator_lifecycle_actions_proxy_to_remote_backend_and_resync_local_state() {
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
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    state
        .apply_remote_state_snapshot(
            &remote.id,
            sample_remote_orchestrator_state(
                "remote-project-1",
                "/remote/repo",
                1,
                OrchestratorInstanceStatus::Running,
            ),
        )
        .expect("initial remote snapshot should apply");
    let local_orchestrator_id = state
        .snapshot()
        .orchestrators
        .into_iter()
        .find(|instance| {
            instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
        })
        .expect("remote orchestrator should be mirrored")
        .id;

    let captured = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured_for_server = captured.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let paused_state = serde_json::to_string(&sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Paused,
    ))
    .expect("paused state should encode");
    let resumed_state = serde_json::to_string(&sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        3,
        OrchestratorInstanceStatus::Running,
    ))
    .expect("resumed state should encode");
    let stopped_state = serde_json::to_string(&sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        4,
        OrchestratorInstanceStatus::Stopped,
    ))
    .expect("stopped state should encode");
    let server = std::thread::spawn(move || {
        let mut action_responses = vec![
            (
                "POST /api/orchestrators/remote-orchestrator-1/pause HTTP/1.1".to_owned(),
                paused_state,
            ),
            (
                "POST /api/orchestrators/remote-orchestrator-1/resume HTTP/1.1".to_owned(),
                resumed_state,
            ),
            (
                "POST /api/orchestrators/remote-orchestrator-1/stop HTTP/1.1".to_owned(),
                stopped_state,
            ),
        ]
        .into_iter();
        for _ in 0..6 {
            let mut stream = accept_test_connection(&listener, "test listener");
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
            let request_head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
            let request_line = request_head
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();
            captured_for_server
                .lock()
                .expect("capture mutex poisoned")
                .push(request_line.clone());

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
                    )
                    .expect("health response should write");
                continue;
            }

            let (expected_request_line, response_body) = action_responses
                .next()
                .expect("action response should still be queued");
            assert_eq!(request_line, expected_request_line);
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                        response_body.len(),
                        response_body
                    )
                    .as_bytes(),
                )
                .expect("state response should write");
        }
    });
    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(false),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(None),
            }),
        );

    let paused = state
        .pause_orchestrator_instance(&local_orchestrator_id)
        .expect("pause should proxy successfully");
    assert_eq!(
        paused
            .orchestrators
            .iter()
            .find(|instance| instance.id == local_orchestrator_id)
            .expect("paused orchestrator should be present")
            .status,
        OrchestratorInstanceStatus::Paused
    );

    let resumed = state
        .resume_orchestrator_instance(&local_orchestrator_id)
        .expect("resume should proxy successfully");
    assert_eq!(
        resumed
            .orchestrators
            .iter()
            .find(|instance| instance.id == local_orchestrator_id)
            .expect("resumed orchestrator should be present")
            .status,
        OrchestratorInstanceStatus::Running
    );

    let stopped = state
        .stop_orchestrator_instance(&local_orchestrator_id)
        .expect("stop should proxy successfully");
    assert_eq!(
        stopped
            .orchestrators
            .iter()
            .find(|instance| instance.id == local_orchestrator_id)
            .expect("stopped orchestrator should be present")
            .status,
        OrchestratorInstanceStatus::Stopped
    );

    assert_eq!(
        captured.lock().expect("capture mutex poisoned").clone(),
        vec![
            "GET /api/health HTTP/1.1".to_owned(),
            "POST /api/orchestrators/remote-orchestrator-1/pause HTTP/1.1".to_owned(),
            "GET /api/health HTTP/1.1".to_owned(),
            "POST /api/orchestrators/remote-orchestrator-1/resume HTTP/1.1".to_owned(),
            "GET /api/health HTTP/1.1".to_owned(),
            "POST /api/orchestrators/remote-orchestrator-1/stop HTTP/1.1".to_owned(),
        ]
    );
    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote snapshot sync keeps the previous remote orchestrators when localization fails.
#[test]
fn remote_snapshot_sync_preserves_existing_orchestrators_when_localization_fails() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut initial_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    );
    let mut second_orchestrator = initial_state.orchestrators[0].clone();
    second_orchestrator.id = "remote-orchestrator-2".to_owned();
    second_orchestrator.status = OrchestratorInstanceStatus::Paused;
    initial_state.orchestrators.push(second_orchestrator);
    state
        .apply_remote_state_snapshot(&remote.id, initial_state)
        .expect("initial remote snapshot should apply");

    let initial_remote_orchestrator_ids = state
        .snapshot()
        .orchestrators
        .into_iter()
        .filter(|instance| instance.remote_id.as_deref() == Some(remote.id.as_str()))
        .filter_map(|instance| instance.remote_orchestrator_id)
        .collect::<HashSet<_>>();
    assert_eq!(
        initial_remote_orchestrator_ids,
        [
            "remote-orchestrator-1".to_owned(),
            "remote-orchestrator-2".to_owned()
        ]
        .into_iter()
        .collect::<HashSet<_>>()
    );

    let mut invalid_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    invalid_state.orchestrators[0].id = "remote-orchestrator-2".to_owned();
    let mut invalid_orchestrator = invalid_state.orchestrators[0].clone();
    invalid_orchestrator.id = "remote-orchestrator-3".to_owned();
    invalid_orchestrator.session_instances[0].session_id = "missing-remote-session".to_owned();
    invalid_state.orchestrators.push(invalid_orchestrator);

    state
        .apply_remote_state_snapshot(&remote.id, invalid_state)
        .expect("remote snapshot should still apply when orchestrator localization fails");

    let remote_orchestrator_ids = state
        .snapshot()
        .orchestrators
        .into_iter()
        .filter(|instance| instance.remote_id.as_deref() == Some(remote.id.as_str()))
        .filter_map(|instance| instance.remote_orchestrator_id)
        .collect::<HashSet<_>>();
    assert_eq!(
        remote_orchestrator_ids,
        [
            "remote-orchestrator-1".to_owned(),
            "remote-orchestrator-2".to_owned()
        ]
        .into_iter()
        .collect::<HashSet<_>>()
    );
    assert!(!remote_orchestrator_ids.contains("remote-orchestrator-3"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote snapshot sync preserves referenced sessions when orchestrator localization fails.
#[test]
fn remote_snapshot_sync_preserves_sessions_referenced_by_existing_orchestrators_when_localization_fails()
 {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let initial_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    );
    state
        .apply_remote_state_snapshot(&remote.id, initial_state)
        .expect("initial remote snapshot should apply");

    let (preserved_local_session_id, preserved_preview) = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("remote mirrored session should exist");
        (
            inner.sessions[index].session.id.clone(),
            inner.sessions[index].session.preview.clone(),
        )
    };

    let mut invalid_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    invalid_state
        .sessions
        .retain(|session| session.id != "remote-session-1");

    state
        .apply_remote_state_snapshot(&remote.id, invalid_state)
        .expect("remote snapshot should still apply when orchestrator localization fails");

    let snapshot = state.snapshot();
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.id == preserved_local_session_id)
            .expect("referenced mirrored session should remain")
            .preview,
        preserved_preview
    );
    let preserved_orchestrator = snapshot
        .orchestrators
        .iter()
        .find(|instance| {
            instance.remote_id.as_deref() == Some(remote.id.as_str())
                && instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
        })
        .expect("existing mirrored orchestrator should remain");
    assert!(
        preserved_orchestrator
            .session_instances
            .iter()
            .any(|instance| instance.session_id == preserved_local_session_id)
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that focused remote state sync rolls back eager proxy-session side effects when orchestrator localization fails.
#[test]
fn focused_remote_state_sync_rolls_back_proxy_sessions_when_orchestrator_localization_fails() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut initial_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    initial_remote_session.preview = "Before focused sync.".to_owned();

    let local_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let local_session_id = upsert_remote_proxy_session_record(
            &mut inner,
            &remote.id,
            &initial_remote_session,
            Some(local_project_id),
        );
        state
            .commit_locked(&mut inner)
            .expect("initial focused remote session should persist");
        local_session_id
    };
    let initial_session_count = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner.sessions.len()
    };

    let mut invalid_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    invalid_state
        .sessions
        .retain(|session| session.id != "remote-session-3");
    invalid_state
        .sessions
        .iter_mut()
        .find(|session| session.id == "remote-session-1")
        .expect("focused session should remain in the payload")
        .preview = "Focused sync updated.".to_owned();

    let target = RemoteSessionTarget {
        local_session_id: local_session_id.clone(),
        remote: remote.clone(),
        remote_session_id: "remote-session-1".to_owned(),
    };
    state
        .sync_remote_state_for_target(&target, invalid_state)
        .expect("focused remote sync should preserve the target session update");

    let snapshot = state.snapshot();
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.id == local_session_id)
            .expect("focused local session should remain")
            .preview,
        "Focused sync updated."
    );
    assert!(
        !snapshot
            .orchestrators
            .iter()
            .any(|instance| { instance.remote_id.as_deref() == Some(remote.id.as_str()) })
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.sessions.len(), initial_session_count);
    assert!(
        inner
            .find_remote_session_index(&remote.id, "remote-session-2")
            .is_none()
    );
    drop(inner);

    let persisted: Value = serde_json::from_slice(
        &fs::read(state.persistence_path.as_path()).expect("persisted state file should exist"),
    )
    .expect("persisted state should deserialize");
    let persisted_sessions = persisted["sessions"]
        .as_array()
        .expect("persisted sessions should be present");
    assert_eq!(persisted_sessions.len(), initial_session_count);
    assert!(!persisted_sessions.iter().any(|candidate| {
        candidate["remoteSessionId"] == Value::String("remote-session-2".to_owned())
    }));
    let persisted_focused = persisted_sessions
        .iter()
        .find(|candidate| {
            candidate["remoteSessionId"] == Value::String("remote-session-1".to_owned())
        })
        .expect("focused mirrored session should persist");
    assert_eq!(
        persisted_focused["session"]["preview"],
        Value::String("Focused sync updated.".to_owned())
    );
    let persisted_orchestrator_instances = persisted["orchestratorInstances"].as_array();
    assert!(persisted_orchestrator_instances.map_or(true, |instances| {
        !instances
            .iter()
            .any(|instance| instance["remoteId"] == Value::String(remote.id.clone()))
    }));
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that focused remote sync ignores stale remote revisions instead of
// rolling an already-mirrored session backward.
#[test]
fn focused_remote_state_sync_skips_stale_revision() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut initial_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    initial_remote_session.preview = "Newest preview.".to_owned();

    let local_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let local_session_id = upsert_remote_proxy_session_record(
            &mut inner,
            &remote.id,
            &initial_remote_session,
            Some(local_project_id),
        );
        state
            .commit_locked(&mut inner)
            .expect("initial focused remote session should persist");
        inner.note_remote_applied_revision(&remote.id, 3);
        local_session_id
    };

    let mut stale_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    stale_state
        .sessions
        .iter_mut()
        .find(|session| session.id == "remote-session-1")
        .expect("focused session should remain in the payload")
        .preview = "Stale preview should be skipped.".to_owned();

    let target = RemoteSessionTarget {
        local_session_id: local_session_id.clone(),
        remote: remote.clone(),
        remote_session_id: "remote-session-1".to_owned(),
    };
    state
        .sync_remote_state_for_target(&target, stale_state)
        .expect("stale focused sync should be ignored");

    let snapshot = state.snapshot();
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.id == local_session_id)
            .expect("focused local session should remain")
            .preview,
        "Newest preview."
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.should_skip_remote_applied_revision(&remote.id, 3));
    assert!(!inner.should_skip_remote_applied_revision(&remote.id, 4));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote snapshot sync removes missing proxy sessions.
#[test]
fn remote_snapshot_sync_removes_missing_proxy_sessions() {
    let state = test_app_state();
    let (kept_local_session_id, removed_local_session_id, local_session_id) = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let kept = inner.create_session(Agent::Codex, None, "/tmp".to_owned(), None, None);
        let removed = inner.create_session(Agent::Codex, None, "/tmp".to_owned(), None, None);
        let local = inner.create_session(Agent::Claude, None, "/tmp".to_owned(), None, None);

        let kept_index = inner
            .find_session_index(&kept.session.id)
            .expect("kept session should exist");
        inner.sessions[kept_index].remote_id = Some("ssh-lab".to_owned());
        inner.sessions[kept_index].remote_session_id = Some("remote-session-keep".to_owned());

        let removed_index = inner
            .find_session_index(&removed.session.id)
            .expect("removed session should exist");
        inner.sessions[removed_index].remote_id = Some("ssh-lab".to_owned());
        inner.sessions[removed_index].remote_session_id = Some("remote-session-gone".to_owned());

        (kept.session.id, removed.session.id, local.session.id)
    };

    let mut remote_state = state.snapshot();
    let mut remote_session = remote_state
        .sessions
        .iter()
        .find(|session| session.id == kept_local_session_id)
        .cloned()
        .expect("kept session should be present in the snapshot");
    remote_session.id = "remote-session-keep".to_owned();
    remote_session.preview = "Remote session still exists.".to_owned();
    remote_state.sessions = vec![remote_session];

    state
        .apply_remote_state_snapshot("ssh-lab", remote_state)
        .expect("remote snapshot should apply");

    let snapshot = state.snapshot();
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.id == kept_local_session_id)
    );
    assert!(
        !snapshot
            .sessions
            .iter()
            .any(|session| session.id == removed_local_session_id)
    );
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.id == local_session_id)
    );
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.id == kept_local_session_id)
            .expect("kept session should remain")
            .preview,
        "Remote session still exists."
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that marked remote fallback payloads dedupe repeated revisions but still
// resync immediately when a newer fallback revision arrives.
#[test]
fn remote_state_event_dedupes_marked_sse_fallback_resyncs_by_revision() {
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
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.preferences.remotes.push(remote.clone());
    }
    let (remote_local_session_id, local_session_id) = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let remote_record = inner.create_session(Agent::Codex, None, "/tmp".to_owned(), None, None);
        let local_record = inner.create_session(Agent::Claude, None, "/tmp".to_owned(), None, None);

        let remote_index = inner
            .find_session_index(&remote_record.session.id)
            .expect("remote session should exist");
        inner.sessions[remote_index].remote_id = Some("ssh-lab".to_owned());
        inner.sessions[remote_index].remote_session_id = Some("remote-session-keep".to_owned());

        (remote_record.session.id, local_record.session.id)
    };

    let mut first_full_state_response = state.snapshot();
    let mut first_remote_session = first_full_state_response
        .sessions
        .iter()
        .find(|session| session.id == remote_local_session_id)
        .cloned()
        .expect("remote session should be present in the snapshot");
    first_remote_session.id = "remote-session-keep".to_owned();
    first_remote_session.preview = "Hydrated from /api/state v1".to_owned();
    first_full_state_response.sessions = vec![first_remote_session];
    let first_full_state_response =
        serde_json::to_string(&first_full_state_response).expect("state response should encode");

    let mut second_full_state_response = state.snapshot();
    let mut second_remote_session = second_full_state_response
        .sessions
        .iter()
        .find(|session| session.id == remote_local_session_id)
        .cloned()
        .expect("remote session should be present in the snapshot");
    second_remote_session.id = "remote-session-keep".to_owned();
    second_remote_session.preview = "Hydrated from /api/state v2".to_owned();
    second_full_state_response.revision = second_full_state_response.revision.saturating_add(1);
    second_full_state_response.sessions = vec![second_remote_session];
    let second_full_state_response =
        serde_json::to_string(&second_full_state_response).expect("state response should encode");

    let captured = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured_for_server = captured.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        let mut state_responses =
            vec![first_full_state_response, second_full_state_response].into_iter();
        for _ in 0..4 {
            let mut stream = accept_test_connection(&listener, "test listener");
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
            let request_head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
            let request_line = request_head
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();
            captured_for_server
                .lock()
                .expect("capture mutex poisoned")
                .push(request_line.clone());

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
                    )
                    .expect("health response should write");
                continue;
            }

            if request_line.starts_with("GET /api/state ") {
                let response = state_responses
                    .next()
                    .expect("state response should still be queued");
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            response.len(),
                            response
                        )
                        .as_bytes(),
                    )
                    .expect("state response should write");
                continue;
            }

            panic!("unexpected request: {request_line}");
        }
    });
    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(false),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(None),
            }),
        );

    let mut first_fallback_payload: Value =
        serde_json::from_str(EMPTY_STATE_EVENTS_PAYLOAD.as_str())
            .expect("fallback payload should parse");
    first_fallback_payload["revision"] = json!(4);
    let first_data_lines = serde_json::to_string_pretty(&first_fallback_payload)
        .expect("first fallback payload should encode")
        .lines()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    let mut second_fallback_payload = first_fallback_payload.clone();
    second_fallback_payload["revision"] = json!(5);
    let second_data_lines = serde_json::to_string_pretty(&second_fallback_payload)
        .expect("second fallback payload should encode")
        .lines()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    dispatch_remote_event(&state, "ssh-lab", "state", &first_data_lines)
        .expect("first fallback state payload should trigger a resync");
    dispatch_remote_event(&state, "ssh-lab", "state", &first_data_lines)
        .expect("duplicate fallback revision should be deduped");
    dispatch_remote_event(&state, "ssh-lab", "state", &second_data_lines)
        .expect("newer fallback revision should trigger another resync");

    let snapshot = state.snapshot();
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.id == remote_local_session_id)
    );
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.id == local_session_id)
    );
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.id == remote_local_session_id)
            .expect("remote mirrored session should remain")
            .preview,
        "Hydrated from /api/state v2"
    );
    assert_eq!(
        captured.lock().expect("capture mutex poisoned").clone(),
        vec![
            "GET /api/health HTTP/1.1".to_owned(),
            "GET /api/state HTTP/1.1".to_owned(),
            "GET /api/health HTTP/1.1".to_owned(),
            "GET /api/state HTTP/1.1".to_owned(),
        ]
    );
    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote fallback resync tracking is per remote, monotonic within a
// single event-stream lifetime, and resettable after disconnects.
#[test]
fn remote_sse_fallback_resync_tracking_is_per_remote_and_monotonic() {
    let state = test_app_state();

    assert!(!state.should_skip_remote_sse_fallback_resync("ssh-lab", 0));
    state.note_remote_sse_fallback_resync("ssh-lab", 0);
    assert!(state.should_skip_remote_sse_fallback_resync("ssh-lab", 0));
    assert!(state.should_skip_remote_sse_fallback_resync("ssh-lab", 0));
    assert!(!state.should_skip_remote_sse_fallback_resync("ssh-lab", 1));
    assert!(!state.should_skip_remote_sse_fallback_resync("ssh-lab-2", 0));

    state.note_remote_sse_fallback_resync("ssh-lab", 1);
    assert!(state.should_skip_remote_sse_fallback_resync("ssh-lab", 1));
    assert!(state.should_skip_remote_sse_fallback_resync("ssh-lab", 0));
    assert!(!state.should_skip_remote_sse_fallback_resync("ssh-lab-2", 1));

    state.clear_remote_sse_fallback_resync("ssh-lab");
    assert!(!state.should_skip_remote_sse_fallback_resync("ssh-lab", 1));
    assert!(!state.should_skip_remote_sse_fallback_resync("ssh-lab", 0));
    assert!(!state.should_skip_remote_sse_fallback_resync("ssh-lab-2", 1));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that StateInner revision tracking handles first insert, same revision,
// higher revision, and lower revision without regressing monotonic ordering.
#[test]
fn state_inner_remote_applied_revision_methods_cover_monotonic_cases() {
    let mut inner = StateInner::new();

    assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 1));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 1));

    inner.note_remote_applied_revision("ssh-lab", 1);
    assert!(inner.should_skip_remote_applied_revision("ssh-lab", 1));
    assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 2));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 1));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 2));

    inner.note_remote_applied_revision("ssh-lab", 4);
    assert!(inner.should_skip_remote_applied_revision("ssh-lab", 4));
    assert!(inner.should_skip_remote_applied_revision("ssh-lab", 3));
    assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 5));
    assert!(inner.should_skip_remote_applied_delta_revision("ssh-lab", 3));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 4));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 5));

    inner.note_remote_applied_revision("ssh-lab", 2);
    assert!(inner.should_skip_remote_applied_revision("ssh-lab", 4));
    assert!(inner.should_skip_remote_applied_revision("ssh-lab", 2));
    assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 5));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 4));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 5));
    assert!(!inner.should_skip_remote_applied_revision("ssh-lab-2", 1));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab-2", 1));
}

// Tests that raw remote error bodies are sanitized and capped before they reach the UI.
#[test]
fn decode_remote_json_sanitizes_and_caps_raw_error_bodies() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let noisy_body = format!(
        "<html>\0Service\tUnavailable\r\nDetails {}{}\u{7}</html>",
        "A".repeat(600),
        "B".repeat(32)
    );
    let response_body_len = noisy_body.as_bytes().len();
    let server = std::thread::spawn(move || {
        let mut stream = accept_test_connection(&listener, "test listener");
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let bytes_read = stream.read(&mut chunk).expect("request should read");
            assert!(bytes_read > 0, "request closed before headers completed");
            buffer.extend_from_slice(&chunk[..bytes_read]);
            if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        stream
            .write_all(
                format!(
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Type: text/html\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                    response_body_len, noisy_body
                )
                .as_bytes(),
            )
            .expect("error response should write");
    });

    let client = BlockingHttpClient::builder()
        .build()
        .expect("test HTTP client should build");
    let response = client
        .get(format!("http://127.0.0.1:{port}/api/health"))
        .send()
        .expect("error response should still be returned");
    let err = match decode_remote_json::<HealthResponse>(response) {
        Ok(_) => panic!("raw non-JSON 503 should surface as an ApiError"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(err.message.contains("Service Unavailable Details"));
    assert!(err.message.chars().count() <= 512);
    assert!(err.message.ends_with("..."));
    assert!(!err.message.contains('\r'));
    assert!(!err.message.contains('\n'));
    assert!(!err.message.contains('\t'));
    assert!(!err.message.chars().any(|ch| ch.is_control()));

    join_test_server(server);
}

// Tests that structured remote JSON error messages are sanitized and capped before they reach the UI.
#[test]
fn decode_remote_json_sanitizes_structured_error_bodies() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let noisy_message = format!(
        "Remote\tfailure\r\n{}{}\u{7}",
        "A".repeat(600),
        "B".repeat(32)
    );
    let response_body = serde_json::to_string(&json!({
        "error": noisy_message,
    }))
    .expect("structured error response should encode");
    let response_body_len = response_body.as_bytes().len();
    let server = std::thread::spawn(move || {
        let mut stream = accept_test_connection(&listener, "test listener");
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let bytes_read = stream.read(&mut chunk).expect("request should read");
            assert!(bytes_read > 0, "request closed before headers completed");
            buffer.extend_from_slice(&chunk[..bytes_read]);
            if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        stream
            .write_all(
                format!(
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                    response_body_len, response_body
                )
                .as_bytes(),
            )
            .expect("error response should write");
    });

    let client = BlockingHttpClient::builder()
        .build()
        .expect("test HTTP client should build");
    let response = client
        .get(format!("http://127.0.0.1:{port}/api/health"))
        .send()
        .expect("error response should still be returned");
    let err = match decode_remote_json::<HealthResponse>(response) {
        Ok(_) => panic!("structured JSON 503 should surface as an ApiError"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(err.message.contains("Remote failure"));
    assert!(err.message.chars().count() <= 512);
    assert!(err.message.ends_with("..."));
    assert!(!err.message.contains('\r'));
    assert!(!err.message.contains('\n'));
    assert!(!err.message.contains('\t'));
    assert!(!err.message.chars().any(|ch| ch.is_control()));

    join_test_server(server);
}

// Tests that oversized remote error bodies are rejected before they are fully decoded into a String.
#[test]
fn decode_remote_json_rejects_oversized_error_bodies() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let oversized_body = "X".repeat(70_000);
    let response_body_len = oversized_body.as_bytes().len();
    let server = std::thread::spawn(move || {
        let mut stream = accept_test_connection(&listener, "test listener");
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let bytes_read = stream.read(&mut chunk).expect("request should read");
            assert!(bytes_read > 0, "request closed before headers completed");
            buffer.extend_from_slice(&chunk[..bytes_read]);
            if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        stream
            .write_all(
                format!(
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Type: text/plain\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                    response_body_len, oversized_body
                )
                .as_bytes(),
            )
            .expect("error response should write");
    });

    let client = BlockingHttpClient::builder()
        .build()
        .expect("test HTTP client should build");
    let response = client
        .get(format!("http://127.0.0.1:{port}/api/health"))
        .send()
        .expect("error response should still be returned");
    let err = match decode_remote_json::<HealthResponse>(response) {
        Ok(_) => panic!("oversized error response should surface as an ApiError"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(err.message, "remote error response too large");

    join_test_server(server);
}

// Tests that applied remote revisions are tracked per remote, stay monotonic,
// and can be reset when an event stream is re-established.
#[test]
fn remote_applied_revision_tracking_is_per_remote_and_monotonic() {
    let state = test_app_state();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 0));
        inner.note_remote_applied_revision("ssh-lab", 0);
        assert!(inner.should_skip_remote_applied_revision("ssh-lab", 0));
        assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 1));
        assert!(!inner.should_skip_remote_applied_revision("ssh-lab-2", 0));

        inner.note_remote_applied_revision("ssh-lab", 2);
        inner.note_remote_applied_revision("ssh-lab", 1);
        assert!(inner.should_skip_remote_applied_revision("ssh-lab", 2));
        assert!(inner.should_skip_remote_applied_revision("ssh-lab", 1));
        assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 3));
        assert!(!inner.should_skip_remote_applied_revision("ssh-lab-2", 2));
    }

    state.clear_remote_applied_revision("ssh-lab");
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 2));
    assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 0));
    assert!(!inner.should_skip_remote_applied_revision("ssh-lab-2", 2));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that non-fallback empty remote state payloads still apply directly.
#[test]
fn remote_state_event_applies_non_fallback_empty_snapshot_payload() {
    let state = test_app_state();
    let (removed_local_session_id, local_session_id) = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let removed = inner.create_session(Agent::Codex, None, "/tmp".to_owned(), None, None);
        let local = inner.create_session(Agent::Claude, None, "/tmp".to_owned(), None, None);

        let removed_index = inner
            .find_session_index(&removed.session.id)
            .expect("remote session should exist");
        inner.sessions[removed_index].remote_id = Some("ssh-lab".to_owned());
        inner.sessions[removed_index].remote_session_id = Some("remote-session-gone".to_owned());

        (removed.session.id, local.session.id)
    };

    let mut remote_state = empty_state_events_response();
    remote_state.revision = 1;
    let data_lines =
        vec![serde_json::to_string(&remote_state).expect("state payload should encode")];
    dispatch_remote_event(&state, "ssh-lab", "state", &data_lines)
        .expect("ordinary empty state payload should apply");

    let snapshot = state.snapshot();
    assert!(
        !snapshot
            .sessions
            .iter()
            .any(|session| session.id == removed_local_session_id)
    );
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.id == local_session_id)
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that remote review put sends scope via query params.
#[test]
fn remote_review_put_sends_scope_via_query_params() {
    let captured = Arc::new(Mutex::new(None::<(String, String)>));
    let captured_for_server = captured.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "test listener");
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

            let request_head = String::from_utf8_lossy(&buffer[..body_start]).to_string();
            let request_line = request_head
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();
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

            if request_line.starts_with("PUT /api/reviews/change-set-1?") {
                *captured_for_server.lock().expect("capture mutex poisoned") =
                    Some((request_line.clone(), body));
                let response = serde_json::to_string(&ReviewDocumentResponse {
                    review_file_path: "/remote/.termal/reviews/change-set-1.json".to_owned(),
                    review: ReviewDocument {
                        version: 1,
                        change_set_id: "change-set-1".to_owned(),
                        revision: 0,
                        origin: None,
                        files: Vec::new(),
                        threads: Vec::new(),
                    },
                })
                .expect("review response should encode");
                let response_bytes = response.as_bytes();
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            response_bytes.len(),
                            response
                        )
                        .as_bytes(),
                    )
                    .expect("review response should write");
                continue;
            }

            panic!("unexpected request: {request_line}");
        }
    });

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
    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(false),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(None),
            }),
        );

    let response: ReviewDocumentResponse = state
        .remote_put_json_with_query_scope(
            &RemoteScope {
                remote,
                remote_project_id: None,
                remote_session_id: Some("remote-session-1".to_owned()),
            },
            "/api/reviews/change-set-1",
            Vec::new(),
            json!({
                "version": 1,
                "changeSetId": "change-set-1",
                "revision": 0,
                "threads": [],
            }),
        )
        .expect("remote review PUT should succeed");

    assert_eq!(
        response.review_file_path,
        "/remote/.termal/reviews/change-set-1.json"
    );
    let (request_line, body) = captured
        .lock()
        .expect("capture mutex poisoned")
        .clone()
        .expect("captured request should exist");
    assert!(request_line.contains("sessionId=remote-session-1"));
    assert!(!request_line.contains("projectId="));
    let parsed_body: Value = serde_json::from_str(&body).expect("review body should decode");
    assert_eq!(parsed_body.get("sessionId"), None);
    assert_eq!(parsed_body.get("projectId"), None);

    join_test_server(server);
}

// Tests that normalize Git repo relative path rejects parent traversal components.
#[test]
fn normalize_git_repo_relative_path_rejects_parent_traversal_components() {
    let error = normalize_git_repo_relative_path("../../etc/passwd")
        .expect_err("parent traversal should be rejected");

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error.message,
        "git file path cannot contain parent-directory traversal"
    );
}

// Tests that rejects projects with unknown remote.
#[test]
fn rejects_projects_with_unknown_remote() {
    let state = test_app_state();

    let error = match state.create_project(CreateProjectRequest {
        name: Some("Remote Project".to_owned()),
        root_path: "/tmp".to_owned(),
        remote_id: "missing-remote".to_owned(),
    }) {
        Ok(_) => panic!("project creation should reject unknown remotes"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("unknown remote"));
}

// Tests that creates sessions for remote projects over SSH.
#[test]
#[ignore = "requires a reachable SSH remote"]
fn creates_sessions_for_remote_projects_over_ssh() {
    let state = test_app_state();

    state
        .update_app_settings(UpdateAppSettingsRequest {
            default_codex_reasoning_effort: None,
            default_claude_approval_mode: None,
            default_claude_effort: None,
            remotes: Some(vec![
                RemoteConfig::local(),
                RemoteConfig {
                    id: "ssh-lab".to_owned(),
                    name: "SSH Lab".to_owned(),
                    transport: RemoteTransport::Ssh,
                    enabled: true,
                    host: Some("example.com".to_owned()),
                    port: Some(22),
                    user: Some("alice".to_owned()),
                },
            ]),
        })
        .unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Remote Project".to_owned()),
            root_path: "/workspace/demo".to_owned(),
            remote_id: "ssh-lab".to_owned(),
        })
        .unwrap();

    let stored_project = project
        .state
        .projects
        .iter()
        .find(|entry| entry.id == project.project_id)
        .expect("created project should be present");
    assert_eq!(stored_project.remote_id, "ssh-lab");

    let error = match state.create_session(CreateSessionRequest {
        agent: Some(Agent::Codex),
        name: Some("Remote Session".to_owned()),
        workdir: None,
        project_id: Some(project.project_id),
        model: None,
        approval_policy: None,
        reasoning_effort: None,
        sandbox_mode: None,
        cursor_mode: None,
        claude_approval_mode: None,
        claude_effort: None,
        gemini_approval_mode: None,
    }) {
        Ok(_) => {
            panic!(
                "remote session creation should require a reachable SSH remote in this integration test"
            )
        }
        Err(error) => error,
    };

    assert!(matches!(
        error.status,
        StatusCode::BAD_GATEWAY | StatusCode::BAD_REQUEST
    ));
    assert!(!error.message.trim().is_empty());
}

// Tests that creates projects and assigns sessions to them.
#[test]
fn creates_projects_and_assigns_sessions_to_them() {
    let state = test_app_state();
    let expected_root = resolve_project_root_path("/tmp").unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: None,
            root_path: "/tmp".to_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    assert_eq!(project.state.projects.len(), 1);

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Project Session".to_owned()),
            workdir: None,
            project_id: Some(project.project_id.clone()),
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let session = created
        .state
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("created session should be present");

    assert_eq!(
        session.project_id.as_deref(),
        Some(project.project_id.as_str())
    );
    assert_eq!(session.workdir, expected_root);
}

// Tests that deleting a project keeps its sessions valid and visible globally.
#[test]
fn deletes_projects_and_unassigns_existing_sessions() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-delete-project-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Delete Project".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Project Session".to_owned()),
            workdir: None,
            project_id: Some(project.project_id.clone()),
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let deleted = state.delete_project(&project.project_id).unwrap();

    assert!(
        deleted
            .projects
            .iter()
            .all(|entry| entry.id != project.project_id)
    );
    let session = deleted
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("created session should remain visible");
    assert_eq!(session.project_id, None);

    fs::remove_dir_all(root).unwrap();
}

// Tests that rejects session workdirs outside the selected project.
#[test]
fn rejects_session_workdirs_outside_the_selected_project() {
    let state = test_app_state();
    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Project".to_owned()),
            root_path: "/tmp".to_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();

    let result = state.create_session(CreateSessionRequest {
        agent: Some(Agent::Codex),
        name: Some("Out of Bounds".to_owned()),
        workdir: Some("/Users".to_owned()),
        project_id: Some(project.project_id),
        model: None,
        approval_policy: None,
        reasoning_effort: None,
        sandbox_mode: None,
        cursor_mode: None,
        claude_approval_mode: None,
        claude_effort: None,
        gemini_approval_mode: None,
    });

    let error = match result {
        Ok(_) => panic!("session workdir outside project should fail"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("must stay inside project"));
}

// Tests that rejects empty project roots.
#[test]
fn rejects_empty_project_roots() {
    let state = test_app_state();

    let result = state.create_project(CreateProjectRequest {
        name: None,
        root_path: "   ".to_owned(),
        remote_id: default_local_remote_id(),
    });
    let error = match result {
        Ok(_) => panic!("empty project path should fail"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, "project root path cannot be empty");
}

// Tests that resolves requested paths inside the session project root.
#[test]
fn resolves_requested_paths_inside_the_session_project_root() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-project-scope-{}", Uuid::new_v4()));
    let inside_dir = root.join("src");
    let inside_file = inside_dir.join("main.rs");
    let outside_root =
        std::env::temp_dir().join(format!("termal-project-scope-outside-{}", Uuid::new_v4()));
    let outside_file = outside_root.join("main.rs");

    fs::create_dir_all(&inside_dir).unwrap();
    fs::create_dir_all(&outside_root).unwrap();
    fs::write(&inside_file, "fn main() {}\n").unwrap();
    fs::write(&outside_file, "fn main() {}\n").unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Scoped Project".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Scoped Session".to_owned()),
            workdir: Some(inside_dir.to_string_lossy().into_owned()),
            project_id: Some(project.project_id),
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let resolved = resolve_session_scoped_requested_path(
        &state,
        &created.session_id,
        &inside_file.to_string_lossy(),
        ScopedPathMode::ExistingFile,
    )
    .unwrap();
    assert_eq!(
        resolved,
        normalize_user_facing_path(&fs::canonicalize(&inside_file).unwrap())
    );

    let error = resolve_session_scoped_requested_path(
        &state,
        &created.session_id,
        &outside_file.to_string_lossy(),
        ScopedPathMode::ExistingFile,
    )
    .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("must stay inside project"));

    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(outside_root).unwrap();
}

// Tests that allows new file paths inside the session project root.
#[test]
fn allows_new_file_paths_inside_the_session_project_root() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-project-write-scope-{}", Uuid::new_v4()));
    let inside_dir = root.join("src");
    let new_file = root.join("generated").join("output.rs");
    let outside_root =
        std::env::temp_dir().join(format!("termal-project-write-outside-{}", Uuid::new_v4()));
    let outside_file = outside_root.join("escape.rs");

    fs::create_dir_all(&inside_dir).unwrap();
    fs::create_dir_all(&outside_root).unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Writable Project".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Writable Session".to_owned()),
            workdir: Some(inside_dir.to_string_lossy().into_owned()),
            project_id: Some(project.project_id),
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let resolved = resolve_session_scoped_requested_path(
        &state,
        &created.session_id,
        &new_file.to_string_lossy(),
        ScopedPathMode::AllowMissingLeaf,
    )
    .unwrap();
    assert_eq!(resolved, new_file);

    let error = resolve_session_scoped_requested_path(
        &state,
        &created.session_id,
        &outside_file.to_string_lossy(),
        ScopedPathMode::AllowMissingLeaf,
    )
    .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("must stay inside project"));

    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(outside_root).unwrap();
}

// Tests that resolves project scoped paths without a session.
#[test]
fn resolves_project_scoped_paths_without_a_session() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-project-scope-only-{}", Uuid::new_v4()));
    let inside_dir = root.join("src");
    let inside_file = inside_dir.join("main.rs");
    let outside_root = std::env::temp_dir().join(format!(
        "termal-project-scope-only-outside-{}",
        Uuid::new_v4()
    ));
    let outside_file = outside_root.join("escape.rs");

    fs::create_dir_all(&inside_dir).unwrap();
    fs::create_dir_all(&outside_root).unwrap();
    fs::write(&inside_file, "fn main() {}\n").unwrap();
    fs::write(&outside_file, "fn main() {}\n").unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Scope Only Project".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();

    let resolved = resolve_project_scoped_requested_path(
        &state,
        None,
        Some(&project.project_id),
        &inside_file.to_string_lossy(),
        ScopedPathMode::ExistingFile,
    )
    .unwrap();
    assert_eq!(
        resolved,
        normalize_user_facing_path(&fs::canonicalize(&inside_file).unwrap())
    );

    let error = resolve_project_scoped_requested_path(
        &state,
        None,
        Some(&project.project_id),
        &outside_file.to_string_lossy(),
        ScopedPathMode::ExistingFile,
    )
    .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("must stay inside project"));

    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(outside_root).unwrap();
}

// Tests that parses quoted Git status paths.
#[test]
fn parses_quoted_git_status_paths() {
    assert_eq!(
        parse_git_status_paths(r#""folder/file with spaces.txt""#),
        (None, "folder/file with spaces.txt".to_owned())
    );
    assert_eq!(
        parse_git_status_paths(r#""caf\303\251.txt""#),
        (None, "caf\u{00e9}.txt".to_owned())
    );
    assert_eq!(
        parse_git_status_paths(r#""old name.txt" -> "new name.txt""#),
        (Some("old name.txt".to_owned()), "new name.txt".to_owned(),)
    );
}

// Tests that Git status file actions support paths with spaces.
#[test]
fn git_status_file_actions_support_paths_with_spaces() {
    let repo_root = std::env::temp_dir().join(format!("termal-git-status-{}", Uuid::new_v4()));
    let nested_dir = repo_root.join("folder");
    let tracked_file = repo_root.join("README.md");
    let spaced_file = nested_dir.join("file with spaces.txt");

    fs::create_dir_all(&nested_dir).unwrap();
    fs::write(&tracked_file, "# Test\n").unwrap();

    run_git_test_command(&repo_root, &["init"]);
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);
    run_git_test_command(&repo_root, &["add", "README.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);

    fs::write(&spaced_file, "hello\n").unwrap();

    let status = load_git_status_for_path(&repo_root).unwrap();
    let file = status
        .files
        .iter()
        .find(|entry| entry.path == "folder/file with spaces.txt")
        .expect("status should include the untracked file");

    assert_eq!(file.index_status.as_deref(), Some("?"));
    assert_eq!(file.worktree_status.as_deref(), Some("?"));

    let pathspecs = collect_git_pathspecs(&file.path, None);
    run_git_pathspec_command(
        &repo_root,
        &["add", "-A"],
        &pathspecs,
        "failed to stage git changes",
    )
    .unwrap();

    let staged_status = load_git_status_for_path(&repo_root).unwrap();
    let staged_file = staged_status
        .files
        .iter()
        .find(|entry| entry.path == "folder/file with spaces.txt")
        .expect("status should include the staged file");

    assert_eq!(staged_file.index_status.as_deref(), Some("A"));
    assert_eq!(staged_file.worktree_status, None);

    run_git_pathspec_command(
        &repo_root,
        &["restore", "--staged"],
        &pathspecs,
        "failed to unstage git changes",
    )
    .unwrap();

    let unstaged_status = load_git_status_for_path(&repo_root).unwrap();
    let unstaged_file = unstaged_status
        .files
        .iter()
        .find(|entry| entry.path == "folder/file with spaces.txt")
        .expect("status should include the unstaged file");

    assert_eq!(unstaged_file.index_status.as_deref(), Some("?"));
    assert_eq!(unstaged_file.worktree_status.as_deref(), Some("?"));

    fs::remove_dir_all(repo_root).unwrap();
}

// Tests that push Git repo updates tracking branch.
#[test]
fn push_git_repo_updates_tracking_branch() {
    let root = std::env::temp_dir().join(format!("termal-git-push-{}", Uuid::new_v4()));
    let remote_root = root.join("remote.git");
    let repo_root = root.join("local");
    let remote_root_string = remote_root.to_string_lossy().into_owned();
    let repo_root_string = repo_root.to_string_lossy().into_owned();

    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&remote_root).unwrap();

    run_git_test_command(&remote_root, &["init", "--bare"]);
    run_git_test_command(
        &root,
        &[
            "clone",
            remote_root_string.as_str(),
            repo_root_string.as_str(),
        ],
    );
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);

    fs::write(repo_root.join("README.md"), "# Init\n").unwrap();
    run_git_test_command(&repo_root, &["add", "README.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);
    run_git_test_command(&repo_root, &["push", "-u", "origin", "HEAD"]);

    fs::write(repo_root.join("README.md"), "# Updated\n").unwrap();
    run_git_test_command(&repo_root, &["commit", "-am", "update"]);

    let response = push_git_repo(&repo_root).unwrap();
    let local_head = run_git_test_command_output(&repo_root, &["rev-parse", "HEAD"]);
    let remote_head = run_git_test_command_output(&remote_root, &["rev-parse", "HEAD"]);

    assert_eq!(local_head, remote_head);
    assert_eq!(response.status.ahead, 0);
    assert_eq!(response.status.behind, 0);
    assert!(response.summary.starts_with("Pushed "));

    fs::remove_dir_all(root).unwrap();
}

// Tests that sync Git repo pulls remote changes.
#[test]
fn sync_git_repo_pulls_remote_changes() {
    let root = std::env::temp_dir().join(format!("termal-git-sync-{}", Uuid::new_v4()));
    let remote_root = root.join("remote.git");
    let repo_root = root.join("local");
    let peer_root = root.join("peer");
    let remote_root_string = remote_root.to_string_lossy().into_owned();
    let repo_root_string = repo_root.to_string_lossy().into_owned();
    let peer_root_string = peer_root.to_string_lossy().into_owned();

    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&remote_root).unwrap();

    run_git_test_command(&remote_root, &["init", "--bare"]);
    run_git_test_command(
        &root,
        &[
            "clone",
            remote_root_string.as_str(),
            repo_root_string.as_str(),
        ],
    );
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);

    fs::write(repo_root.join("README.md"), "# Init\n").unwrap();
    run_git_test_command(&repo_root, &["add", "README.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);
    run_git_test_command(&repo_root, &["push", "-u", "origin", "HEAD"]);

    run_git_test_command(
        &root,
        &[
            "clone",
            remote_root_string.as_str(),
            peer_root_string.as_str(),
        ],
    );
    run_git_test_command(&peer_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&peer_root, &["config", "user.name", "TermAl"]);
    fs::write(peer_root.join("README.md"), "# Peer\n").unwrap();
    run_git_test_command(&peer_root, &["commit", "-am", "peer update"]);
    run_git_test_command(&peer_root, &["push"]);

    let response = sync_git_repo(&repo_root).unwrap();
    let local_head = run_git_test_command_output(&repo_root, &["rev-parse", "HEAD"]);
    let remote_head = run_git_test_command_output(&remote_root, &["rev-parse", "HEAD"]);

    assert_eq!(
        fs::read_to_string(repo_root.join("README.md"))
            .unwrap()
            .replace("\r\n", "\n"),
        "# Peer\n",
    );
    assert_eq!(local_head, remote_head);
    assert_eq!(response.status.ahead, 0);
    assert_eq!(response.status.behind, 0);
    assert!(response.summary.starts_with("Synced "));

    fs::remove_dir_all(root).unwrap();
}

// Tests that project scoped paths require a session or project identifier.
#[test]
fn project_scoped_paths_require_a_session_or_project_identifier() {
    let state = test_app_state();
    let error = resolve_project_scoped_requested_path(
        &state,
        None,
        None,
        "/tmp",
        ScopedPathMode::ExistingPath,
    )
    .unwrap_err();

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, "sessionId or projectId is required");
}

// Tests that read directory accepts project ID without session.
#[tokio::test]
async fn read_directory_accepts_project_id_without_session() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-project-fs-read-{}", Uuid::new_v4()));
    let src_dir = root.join("src");
    let file_path = src_dir.join("main.rs");

    fs::create_dir_all(&src_dir).unwrap();
    fs::write(
        &file_path,
        "fn main() {}
",
    )
    .unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Project Files".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();

    let Json(response) = read_directory(
        State(state),
        Query(FileQuery {
            path: root.to_string_lossy().into_owned(),
            project_id: Some(project.project_id),
            session_id: None,
        }),
    )
    .await
    .unwrap();

    assert_eq!(
        response.path,
        normalize_user_facing_path(&fs::canonicalize(&root).unwrap()).to_string_lossy()
    );
    assert_eq!(response.entries.len(), 1);
    assert_eq!(response.entries[0].name, "src");

    fs::remove_dir_all(root).unwrap();
}

// Tests that API router sets local CORS headers.
#[tokio::test]
async fn api_router_sets_local_cors_headers() {
    let response = app_router(test_app_state())
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .header(axum::http::header::ORIGIN, "http://127.0.0.1:8787")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("request should complete");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN),
        Some(&HeaderValue::from_static("http://127.0.0.1:8787")),
    );
}

// Tests that health route reports inline orchestrator template compatibility.
#[tokio::test]
async fn health_route_reports_inline_orchestrator_template_support() {
    let (status, response): (StatusCode, Value) = request_json(
        &app_router(test_app_state()),
        Request::builder()
            .method("GET")
            .uri("/api/health")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        response,
        json!({
            "ok": true,
            "supportsInlineOrchestratorTemplates": true,
        })
    );
}

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

#[test]
fn annotate_remote_terminal_429_prefixes_only_throttled_remote_errors() {
    let throttled = annotate_remote_terminal_429(
        ApiError::from_status(
            StatusCode::TOO_MANY_REQUESTS,
            "too many local terminal commands are already running; limit is 4",
        ),
        "SSH Terminal Limit",
    );
    assert_eq!(throttled.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        throttled.message,
        "remote SSH Terminal Limit: too many local terminal commands are already running; limit is 4"
    );

    let server_error = annotate_remote_terminal_429(
        ApiError::from_status(StatusCode::INTERNAL_SERVER_ERROR, "remote server exploded"),
        "SSH Terminal Limit",
    );
    assert_eq!(server_error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(server_error.message, "remote server exploded");
}

#[tokio::test]
async fn terminal_run_route_proxies_valid_remote_multibyte_commands() {
    let captured_body = Arc::new(Mutex::new(None::<String>));
    let captured_for_server = captured_body.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let command = format!("#{}", "\u{00e9}".repeat(TERMINAL_COMMAND_MAX_CHARS - 1));
    let remote_response = serde_json::to_string(&TerminalCommandResponse {
        command: command.clone(),
        duration_ms: 12,
        exit_code: Some(0),
        output_truncated: false,
        shell: "sh".to_owned(),
        stderr: String::new(),
        stdout: "ok\n".to_owned(),
        success: true,
        timed_out: false,
        workdir: "/remote/repo".to_owned(),
    })
    .expect("terminal response should encode");
    let server = std::thread::spawn(move || {
        // Loop until the terminal run request is captured rather than
        // hard-coding the number of proxy round-trips. A future change that
        // adds a capability probe, a binding step, or a retry would
        // otherwise produce a confusing dual failure (server thread
        // panicking on the accept deadline AND the proxy's next request
        // hitting a closed listener). This loop tolerates any number of
        // pre-run requests and terminates as soon as the terminal/run
        // request has been served.
        loop {
            let mut stream = accept_test_connection(&listener, "test listener");
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
                *captured_for_server.lock().expect("capture mutex poisoned") = Some(body);
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            remote_response.len(),
                            remote_response
                        )
                        .as_bytes(),
                    )
                    .expect("terminal response should write");
                break;
            }

            panic!("unexpected request: {request_line}");
        }
    });

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
    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: Mutex::new(remote.clone()),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(true),
                event_bridge_shutdown: AtomicBool::new(false),
                supports_inline_orchestrator_templates: Mutex::new(None),
            }),
        );
    let app = app_router(state);

    let (status, response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": command,
                    "projectId": project_id,
                    "workdir": " /remote/repo ",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        response["command"].as_str().unwrap().chars().count(),
        TERMINAL_COMMAND_MAX_CHARS
    );
    let captured: Value = serde_json::from_str(
        captured_body
            .lock()
            .expect("capture mutex poisoned")
            .as_ref()
            .expect("remote request should be captured"),
    )
    .expect("remote request body should decode");
    assert_eq!(
        captured["workdir"],
        Value::String("/remote/repo".to_owned())
    );
    assert_eq!(
        captured["projectId"],
        Value::String("remote-project-1".to_owned())
    );
    assert_eq!(
        captured["command"].as_str().unwrap().chars().count(),
        TERMINAL_COMMAND_MAX_CHARS
    );

    join_test_server(server);
}

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

// Tests that read and write file accept project ID without session.
#[tokio::test]
async fn read_and_write_file_accept_project_id_without_session() {
    let state = test_app_state();
    let root =
        std::env::temp_dir().join(format!("termal-project-file-read-write-{}", Uuid::new_v4()));
    let existing_file = root.join("src").join("main.rs");
    let new_file = root.join("generated").join("output.rs");

    fs::create_dir_all(existing_file.parent().unwrap()).unwrap();
    fs::write(
        &existing_file,
        "fn main() {}
",
    )
    .unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Project Files".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();

    let Json(read_response) = read_file(
        State(state.clone()),
        Query(FileQuery {
            path: existing_file.to_string_lossy().into_owned(),
            project_id: Some(project.project_id.clone()),
            session_id: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        read_response.content,
        "fn main() {}
"
    );
    assert_eq!(
        read_response.content_hash.as_deref(),
        Some(file_content_hash(read_response.content.as_bytes()).as_str())
    );
    assert_eq!(
        read_response.size_bytes,
        Some(read_response.content.len() as u64)
    );

    let Json(write_response) = write_file(
        State(state),
        Json(WriteFileRequest {
            path: new_file.to_string_lossy().into_owned(),
            content: "pub fn generated() {}
"
            .to_owned(),
            base_hash: None,
            overwrite: false,
            project_id: Some(project.project_id),
            session_id: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(write_response.path, new_file.to_string_lossy());
    assert_eq!(
        fs::read_to_string(&new_file).unwrap(),
        "pub fn generated() {}
"
    );
    assert_eq!(
        write_response.content_hash.as_deref(),
        Some(file_content_hash(write_response.content.as_bytes()).as_str())
    );

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn write_file_rejects_missing_path_traversal_outside_project() {
    let state = test_app_state();
    let root =
        std::env::temp_dir().join(format!("termal-project-file-traversal-{}", Uuid::new_v4()));
    let outside_root = std::env::temp_dir().join(format!(
        "termal-project-file-traversal-outside-{}",
        Uuid::new_v4()
    ));
    let outside_file = outside_root.join("escape.rs");
    let traversal_file = root
        .join("missing")
        .join("..")
        .join("..")
        .join(
            outside_root
                .file_name()
                .expect("outside root should have a name"),
        )
        .join("escape.rs");

    fs::create_dir_all(&root).unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Project Files".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();

    let error = match write_file(
        State(state),
        Json(WriteFileRequest {
            path: traversal_file.to_string_lossy().into_owned(),
            content: "pub fn escape() {}\n".to_owned(),
            base_hash: None,
            overwrite: false,
            project_id: Some(project.project_id),
            session_id: None,
        }),
    )
    .await
    {
        Ok(_) => panic!("traversal write should fail"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(
        error
            .message
            .contains("cannot contain unresolved `.` or `..`")
            || error.message.contains("must stay inside project")
    );
    assert!(!outside_file.exists());
    fs::remove_dir_all(root).unwrap();
    if outside_root.exists() {
        fs::remove_dir_all(outside_root).unwrap();
    }
}

// Tests that write file rejects stale editor base hashes.
#[tokio::test]
async fn write_file_rejects_stale_base_hash() {
    let state = test_app_state();
    let root =
        std::env::temp_dir().join(format!("termal-project-file-stale-base-{}", Uuid::new_v4()));
    let existing_file = root.join("src").join("main.rs");

    fs::create_dir_all(existing_file.parent().unwrap()).unwrap();
    fs::write(&existing_file, "fn main() {}\n").unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Project Files".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();

    let Json(read_response) = read_file(
        State(state.clone()),
        Query(FileQuery {
            path: existing_file.to_string_lossy().into_owned(),
            project_id: Some(project.project_id.clone()),
            session_id: None,
        }),
    )
    .await
    .unwrap();
    let base_hash = read_response
        .content_hash
        .expect("read response should include a content hash");

    fs::write(&existing_file, "fn main() { println!(\"agent\"); }\n").unwrap();

    let error = match write_file(
        State(state.clone()),
        Json(WriteFileRequest {
            path: existing_file.to_string_lossy().into_owned(),
            content: "fn main() { println!(\"user\"); }\n".to_owned(),
            base_hash: Some(base_hash),
            overwrite: false,
            project_id: Some(project.project_id.clone()),
            session_id: None,
        }),
    )
    .await
    {
        Ok(_) => panic!("stale file write should fail"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("file changed on disk before save"));
    assert_eq!(
        fs::read_to_string(&existing_file).unwrap(),
        "fn main() { println!(\"agent\"); }\n"
    );

    let Json(overwrite_response) = write_file(
        State(state),
        Json(WriteFileRequest {
            path: existing_file.to_string_lossy().into_owned(),
            content: "fn main() { println!(\"user\"); }\n".to_owned(),
            base_hash: Some("sha256:stale".to_owned()),
            overwrite: true,
            project_id: Some(project.project_id),
            session_id: None,
        }),
    )
    .await
    .unwrap();

    assert_eq!(
        overwrite_response.content,
        "fn main() { println!(\"user\"); }\n"
    );
    assert_eq!(
        fs::read_to_string(&existing_file).unwrap(),
        "fn main() { println!(\"user\"); }\n"
    );

    fs::remove_dir_all(root).unwrap();
}

// Tests that watcher changes are summarized for the active local agent turn.
#[test]
fn active_turn_file_changes_are_summarized_on_record() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!(
        "termal-active-turn-file-changes-{}",
        Uuid::new_v4()
    ));
    let changed_file = root.join("src").join("main.rs");
    let ignored_file = std::env::temp_dir().join(format!(
        "termal-active-turn-file-changes-outside-{}.rs",
        Uuid::new_v4()
    ));

    fs::create_dir_all(changed_file.parent().unwrap()).unwrap();
    fs::write(&changed_file, "fn main() {}\n").unwrap();
    fs::write(&ignored_file, "pub fn outside() {}\n").unwrap();

    let session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Files".to_owned()),
            root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].active_turn_start_message_count =
            Some(inner.sessions[index].session.messages.len());
        session_id
    };

    state.record_active_turn_file_changes(&[
        WorkspaceFileChangeEvent {
            path: changed_file.to_string_lossy().into_owned(),
            kind: WorkspaceFileChangeKind::Modified,
            root_path: None,
            session_id: None,
            mtime_ms: None,
            size_bytes: None,
        },
        WorkspaceFileChangeEvent {
            path: ignored_file.to_string_lossy().into_owned(),
            kind: WorkspaceFileChangeKind::Modified,
            root_path: None,
            session_id: None,
            mtime_ms: None,
            size_bytes: None,
        },
    ]);

    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(&session_id).unwrap();
    assert_eq!(
        inner.sessions[index].active_turn_file_changes.len(),
        1,
        "only files under the session workdir should be tracked",
    );

    let message_id = inner.next_message_id();
    assert!(push_active_turn_file_changes_on_record(
        &mut inner.sessions[index],
        message_id,
    ));
    assert!(inner.sessions[index].active_turn_file_changes.is_empty());
    match inner.sessions[index].session.messages.last() {
        Some(Message::FileChanges { title, files, .. }) => {
            assert_eq!(title, "Agent changed 1 file");
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].path, changed_file.to_string_lossy());
            assert_eq!(files[0].kind, WorkspaceFileChangeKind::Modified);
        }
        other => panic!("expected file changes message, got {other:?}"),
    }

    drop(inner);
    fs::remove_dir_all(root).unwrap();
    fs::remove_file(ignored_file).unwrap();
}

#[test]
fn active_turn_file_changes_prefer_session_scoped_watcher_hints() {
    let state = test_app_state();
    let root =
        std::env::temp_dir().join(format!("termal-active-turn-file-scope-{}", Uuid::new_v4()));
    let changed_file = root.join("src").join("main.rs");
    fs::create_dir_all(changed_file.parent().unwrap()).unwrap();
    fs::write(&changed_file, "fn main() {}\n").unwrap();

    let (first_session_id, second_session_id) = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let first = inner.create_session(
            Agent::Codex,
            Some("First".to_owned()),
            root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let first_session_id = first.session.id.clone();
        let second = inner.create_session(
            Agent::Codex,
            Some("Second".to_owned()),
            root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let second_session_id = second.session.id.clone();
        for session_id in [&first_session_id, &second_session_id] {
            let index = inner.find_session_index(session_id).unwrap();
            inner.sessions[index].active_turn_start_message_count =
                Some(inner.sessions[index].session.messages.len());
        }
        (first_session_id, second_session_id)
    };

    state.record_active_turn_file_changes(&[
        WorkspaceFileChangeEvent {
            path: changed_file.to_string_lossy().into_owned(),
            kind: WorkspaceFileChangeKind::Modified,
            root_path: Some(root.to_string_lossy().into_owned()),
            session_id: None,
            mtime_ms: None,
            size_bytes: None,
        },
        WorkspaceFileChangeEvent {
            path: changed_file.to_string_lossy().into_owned(),
            kind: WorkspaceFileChangeKind::Modified,
            root_path: Some(root.to_string_lossy().into_owned()),
            session_id: Some(first_session_id.clone()),
            mtime_ms: None,
            size_bytes: None,
        },
    ]);

    let inner = state.inner.lock().expect("state mutex poisoned");
    let first = inner
        .sessions
        .iter()
        .find(|record| record.session.id == first_session_id)
        .expect("first session should exist");
    let second = inner
        .sessions
        .iter()
        .find(|record| record.session.id == second_session_id)
        .expect("second session should exist");
    assert_eq!(first.active_turn_file_changes.len(), 1);
    assert!(second.active_turn_file_changes.is_empty());
    drop(inner);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn merge_workspace_file_change_kind_treats_delete_create_as_modified() {
    assert_eq!(
        merge_workspace_file_change_kind(
            WorkspaceFileChangeKind::Deleted,
            WorkspaceFileChangeKind::Created,
        ),
        WorkspaceFileChangeKind::Modified,
    );
    assert_eq!(
        merge_workspace_file_change_kind(
            WorkspaceFileChangeKind::Created,
            WorkspaceFileChangeKind::Deleted,
        ),
        WorkspaceFileChangeKind::Modified,
    );
}

fn canonical_test_watch_path(path: &FsPath) -> PathBuf {
    normalize_user_facing_path(&fs::canonicalize(path).expect("test path should canonicalize"))
}

#[test]
fn workspace_file_watch_scopes_include_project_and_session_roots() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-watch-scopes-{}", Uuid::new_v4()));
    let project_root = root.join("project");
    let session_root = root.join("session");
    fs::create_dir_all(&project_root).unwrap();
    fs::create_dir_all(&session_root).unwrap();

    create_test_project(&state, &project_root, "Watch Project");
    let session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Watch Session".to_owned()),
            session_root.to_string_lossy().into_owned(),
            None,
            None,
        );
        record.session.id
    };

    let scopes = collect_workspace_file_watch_scopes(&state)
        .into_iter()
        .map(|scope| (scope.root_path, scope.session_id))
        .collect::<Vec<_>>();

    assert!(scopes.contains(&(canonical_test_watch_path(&project_root), None)));
    assert!(scopes.contains(&(canonical_test_watch_path(&session_root), Some(session_id),)));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn workspace_file_watch_roots_prune_nested_roots() {
    let root = std::env::temp_dir().join(format!("termal-watch-nested-{}", Uuid::new_v4()));
    let nested = root.join("packages").join("app");
    fs::create_dir_all(&nested).unwrap();
    let root = canonical_test_watch_path(&root);
    let nested = canonical_test_watch_path(&nested);

    assert_eq!(
        prune_nested_workspace_file_watch_roots(vec![nested.clone(), root.clone()]),
        vec![root.clone()],
    );
    assert_eq!(
        prune_nested_workspace_file_watch_roots(vec![root.clone(), nested]),
        vec![root.clone()],
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn workspace_file_changes_from_path_uses_specific_unique_scopes() {
    let root = std::env::temp_dir().join(format!("termal-watch-change-{}", Uuid::new_v4()));
    let nested = root.join("packages").join("app");
    let changed_file = nested.join("src").join("main.rs");
    fs::create_dir_all(changed_file.parent().unwrap()).unwrap();
    fs::write(&changed_file, "fn main() {}\n").unwrap();
    let root_path = canonical_test_watch_path(&root);
    let nested_path = canonical_test_watch_path(&nested);
    let changed_path = canonical_test_watch_path(&changed_file)
        .to_string_lossy()
        .into_owned();

    let changes = workspace_file_changes_from_path(
        &changed_file,
        WorkspaceFileChangeKind::Modified,
        &[
            WorkspaceFileWatchScope {
                root_path: root_path.clone(),
                session_id: None,
            },
            WorkspaceFileWatchScope {
                root_path: nested_path.clone(),
                session_id: Some("session-1".to_owned()),
            },
            WorkspaceFileWatchScope {
                root_path: nested_path.clone(),
                session_id: Some("session-1".to_owned()),
            },
            WorkspaceFileWatchScope {
                root_path: nested_path.clone(),
                session_id: Some("session-2".to_owned()),
            },
        ],
    );

    assert_eq!(changes.len(), 3);
    assert!(changes.iter().all(|change| change.path == changed_path));
    assert!(changes.iter().any(|change| {
        change.root_path.as_deref() == Some(nested_path.to_string_lossy().as_ref())
            && change.session_id.as_deref() == Some("session-1")
    }));
    assert!(changes.iter().any(|change| {
        change.root_path.as_deref() == Some(nested_path.to_string_lossy().as_ref())
            && change.session_id.as_deref() == Some("session-2")
    }));
    assert!(changes.iter().any(|change| {
        change.root_path.as_deref() == Some(root_path.to_string_lossy().as_ref())
            && change.session_id.is_none()
    }));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn workspace_file_changes_from_path_emits_unscoped_fallback() {
    let root = std::env::temp_dir().join(format!("termal-watch-fallback-{}", Uuid::new_v4()));
    let changed_file = root.join("generated.rs");
    fs::create_dir_all(&root).unwrap();
    fs::write(&changed_file, "fn generated() {}\n").unwrap();
    let changed_path = canonical_test_watch_path(&changed_file)
        .to_string_lossy()
        .into_owned();

    let changes =
        workspace_file_changes_from_path(&changed_file, WorkspaceFileChangeKind::Created, &[]);

    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].path, changed_path);
    assert_eq!(changes[0].kind, WorkspaceFileChangeKind::Created);
    assert_eq!(changes[0].root_path, None);
    assert_eq!(changes[0].session_id, None);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn late_turn_file_changes_are_summarized_during_grace_window() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-late-file-change-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let changed_file = root.join("generated.rs");
    fs::write(&changed_file, "fn generated() {}\n").unwrap();

    let session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Files".to_owned()),
            root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].active_turn_start_message_count =
            Some(inner.sessions[index].session.messages.len());
        finish_active_turn_file_change_tracking(&mut inner.sessions[index]);
        session_id
    };

    state.record_active_turn_file_changes(&[WorkspaceFileChangeEvent {
        path: changed_file.to_string_lossy().into_owned(),
        kind: WorkspaceFileChangeKind::Created,
        root_path: Some(root.to_string_lossy().into_owned()),
        session_id: Some(session_id.clone()),
        mtime_ms: None,
        size_bytes: None,
    }]);

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should exist");
    let files = session
        .messages
        .iter()
        .find_map(|message| match message {
            Message::FileChanges { files, .. } => Some(files),
            _ => None,
        })
        .expect("late watcher event should create a file-change summary");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].kind, WorkspaceFileChangeKind::Created);
    assert_eq!(files[0].path, changed_file.to_string_lossy());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn expired_late_turn_file_change_grace_window_does_not_emit_summary() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!(
        "termal-expired-late-file-change-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root).unwrap();
    let changed_file = root.join("generated.rs");
    fs::write(&changed_file, "fn generated() {}\n").unwrap();

    let session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Expired Files".to_owned()),
            root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].active_turn_file_change_grace_deadline =
            Some(std::time::Instant::now() - Duration::from_millis(1));
        session_id
    };

    state.record_active_turn_file_changes(&[WorkspaceFileChangeEvent {
        path: changed_file.to_string_lossy().into_owned(),
        kind: WorkspaceFileChangeKind::Created,
        root_path: Some(root.to_string_lossy().into_owned()),
        session_id: Some(session_id.clone()),
        mtime_ms: None,
        size_bytes: None,
    }]);

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("session should exist");
    assert!(record.active_turn_file_change_grace_deadline.is_none());
    assert!(record.active_turn_file_changes.is_empty());
    assert!(
        record
            .session
            .messages
            .iter()
            .all(|message| !matches!(message, Message::FileChanges { .. }))
    );
    drop(inner);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn idle_finish_active_turn_file_change_tracking_does_not_open_grace_window() {
    let mut inner = StateInner::new();
    let mut record = inner.create_session(
        Agent::Codex,
        Some("Idle Files".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    record.active_turn_file_changes.insert(
        "/tmp/generated.rs".to_owned(),
        WorkspaceFileChangeKind::Created,
    );

    finish_active_turn_file_change_tracking(&mut record);

    assert!(record.active_turn_start_message_count.is_none());
    assert!(record.active_turn_file_change_grace_deadline.is_none());
    assert!(record.active_turn_file_changes.is_empty());
}

#[test]
fn late_turn_file_change_grace_window_emits_only_once() {
    let state = test_app_state();
    let root =
        std::env::temp_dir().join(format!("termal-late-file-change-once-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let first_file = root.join("first.rs");
    let second_file = root.join("second.rs");
    fs::write(&first_file, "fn first() {}\n").unwrap();
    fs::write(&second_file, "fn second() {}\n").unwrap();

    let session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Files".to_owned()),
            root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].active_turn_start_message_count =
            Some(inner.sessions[index].session.messages.len());
        finish_active_turn_file_change_tracking(&mut inner.sessions[index]);
        session_id
    };

    state.record_active_turn_file_changes(&[WorkspaceFileChangeEvent {
        path: first_file.to_string_lossy().into_owned(),
        kind: WorkspaceFileChangeKind::Created,
        root_path: Some(root.to_string_lossy().into_owned()),
        session_id: Some(session_id.clone()),
        mtime_ms: None,
        size_bytes: None,
    }]);
    state.record_active_turn_file_changes(&[WorkspaceFileChangeEvent {
        path: second_file.to_string_lossy().into_owned(),
        kind: WorkspaceFileChangeKind::Created,
        root_path: Some(root.to_string_lossy().into_owned()),
        session_id: Some(session_id.clone()),
        mtime_ms: None,
        size_bytes: None,
    }]);

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should exist");
    let file_change_messages = session
        .messages
        .iter()
        .filter(|message| matches!(message, Message::FileChanges { .. }))
        .count();
    assert_eq!(file_change_messages, 1);
    fs::remove_dir_all(root).unwrap();
}

// Tests that read file returns not found for missing project file.
#[tokio::test]
async fn read_file_returns_not_found_for_missing_project_file() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-project-file-missing-{}", Uuid::new_v4()));
    let missing_file = root.join("missing.rs");

    fs::create_dir_all(&root).unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Project Files".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();

    let error = match read_file(
        State(state),
        Query(FileQuery {
            path: missing_file.to_string_lossy().into_owned(),
            project_id: Some(project.project_id),
            session_id: None,
        }),
    )
    .await
    {
        Ok(_) => panic!("missing file read should fail"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert!(error.message.contains("file not found"));

    fs::remove_dir_all(root).unwrap();
}

// Tests that read directory returns not found for missing project path.
#[tokio::test]
async fn read_directory_returns_not_found_for_missing_project_path() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!(
        "termal-project-directory-missing-{}",
        Uuid::new_v4()
    ));
    let missing_dir = root.join("missing");

    fs::create_dir_all(&root).unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Project Files".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();

    let error = match read_directory(
        State(state),
        Query(FileQuery {
            path: missing_dir.to_string_lossy().into_owned(),
            project_id: Some(project.project_id),
            session_id: None,
        }),
    )
    .await
    {
        Ok(_) => panic!("missing directory read should fail"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert!(error.message.contains("path not found"));

    fs::remove_dir_all(root).unwrap();
}

// Tests that read instruction document returns not found for missing file.
#[test]
fn read_instruction_document_returns_not_found_for_missing_file() {
    let workdir =
        std::env::temp_dir().join(format!("termal-instruction-missing-{}", Uuid::new_v4()));
    let missing_file = workdir.join("AGENTS.md");

    fs::create_dir_all(&workdir).unwrap();

    let error = read_instruction_document(&missing_file, &workdir)
        .expect_err("missing instruction file should fail");

    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert!(error.message.contains("instruction file not found"));

    fs::remove_dir_all(workdir).unwrap();
}

// Tests that read file rejects content over size limit.
#[tokio::test]
async fn read_file_rejects_content_over_size_limit() {
    let state = test_app_state();
    let root =
        std::env::temp_dir().join(format!("termal-project-file-read-limit-{}", Uuid::new_v4()));
    let oversized_file = root.join("big.txt");

    fs::create_dir_all(&root).unwrap();
    fs::write(&oversized_file, "a".repeat(MAX_FILE_CONTENT_BYTES + 1)).unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Project Files".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();

    let error = match read_file(
        State(state),
        Query(FileQuery {
            path: oversized_file.to_string_lossy().into_owned(),
            project_id: Some(project.project_id),
            session_id: None,
        }),
    )
    .await
    {
        Ok(_) => panic!("oversized read should fail"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("read limit"));

    fs::remove_dir_all(root).unwrap();
}

// Tests that write file rejects content over size limit.
#[tokio::test]
async fn write_file_rejects_content_over_size_limit() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!(
        "termal-project-file-write-limit-{}",
        Uuid::new_v4()
    ));
    let output_file = root.join("generated").join("output.rs");
    let oversized_content = "b".repeat(MAX_FILE_CONTENT_BYTES + 1);

    fs::create_dir_all(&root).unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Project Files".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();

    let error = match write_file(
        State(state),
        Json(WriteFileRequest {
            path: output_file.to_string_lossy().into_owned(),
            content: oversized_content,
            base_hash: None,
            overwrite: false,
            project_id: Some(project.project_id),
            session_id: None,
        }),
    )
    .await
    {
        Ok(_) => panic!("oversized write should fail"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("write limit"));
    assert!(!output_file.exists());

    fs::remove_dir_all(root).unwrap();
}

// Tests that project digest surfaces pending approval actions.
#[test]
fn project_digest_surfaces_pending_approval_actions() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-project-digest-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();

    let project_id = create_test_project(&state, &root, "Digest Project");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);

    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Implemented the requested fix.".to_owned(),
                expanded_text: None,
            },
        )
        .unwrap();

    let approval_message_id = state.allocate_message_id();
    state
        .push_message(
            &session_id,
            Message::Approval {
                id: approval_message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Approve command".to_owned(),
                command: "cargo test".to_owned(),
                command_language: Some(shell_language().to_owned()),
                detail: "Approval required.".to_owned(),
                decision: ApprovalDecision::Pending,
            },
        )
        .unwrap();
    state
        .register_codex_pending_approval(
            &session_id,
            approval_message_id.clone(),
            CodexPendingApproval {
                kind: CodexApprovalKind::CommandExecution,
                request_id: json!("req-project-digest"),
            },
        )
        .unwrap();

    let digest = state.project_digest(&project_id).unwrap();
    let action_ids = digest
        .proposed_actions
        .iter()
        .map(|action| action.id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        digest.primary_session_id.as_deref(),
        Some(session_id.as_str())
    );
    assert_eq!(digest.current_status, "Waiting on your decision.");
    assert_eq!(digest.done_summary, "Implemented the requested fix.");
    assert_eq!(digest.source_message_ids[0], approval_message_id);
    assert_eq!(action_ids, vec!["approve", "reject", "review-in-termal"]);

    fs::remove_dir_all(root).unwrap();
}

// Tests that project digest prefers review actions for dirty idle project.
#[test]
fn project_digest_prefers_review_actions_for_dirty_idle_project() {
    let state = test_app_state();
    let repo_root = std::env::temp_dir().join(format!("termal-project-review-{}", Uuid::new_v4()));
    fs::create_dir_all(repo_root.join("src")).unwrap();
    fs::write(
        repo_root.join("src/lib.rs"),
        "pub fn value() -> u32 { 1 }\n",
    )
    .unwrap();

    run_git_test_command(&repo_root, &["init"]);
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);
    run_git_test_command(&repo_root, &["add", "."]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);

    fs::write(
        repo_root.join("src/lib.rs"),
        "pub fn value() -> u32 { 2 }\n",
    )
    .unwrap();

    let project_id = create_test_project(&state, &repo_root, "Review Project");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &repo_root);

    let digest = state.project_digest(&project_id).unwrap();
    let action_ids = digest
        .proposed_actions
        .iter()
        .map(|action| action.id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        digest.primary_session_id.as_deref(),
        Some(session_id.as_str())
    );
    assert_eq!(digest.current_status, "Changes are ready for review.");
    assert!(digest.done_summary.contains("1 changed file"));
    assert_eq!(
        action_ids,
        vec!["review-in-termal", "ask-agent-to-commit", "keep-iterating"]
    );

    fs::remove_dir_all(repo_root).unwrap();
}

// Tests that project action approve routes to the live project approval.
#[test]
fn project_action_approve_routes_to_the_live_project_approval() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-project-approve-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();

    let project_id = create_test_project(&state, &root, "Approval Project");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    let (runtime, input_rx) = test_codex_runtime_handle("project-approve");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }

    let approval_message_id = state.allocate_message_id();
    state
        .push_message(
            &session_id,
            Message::Approval {
                id: approval_message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Approve command".to_owned(),
                command: "cargo test".to_owned(),
                command_language: Some(shell_language().to_owned()),
                detail: "Approval required.".to_owned(),
                decision: ApprovalDecision::Pending,
            },
        )
        .unwrap();
    state
        .register_codex_pending_approval(
            &session_id,
            approval_message_id.clone(),
            CodexPendingApproval {
                kind: CodexApprovalKind::CommandExecution,
                request_id: json!("req-project-approve"),
            },
        )
        .unwrap();

    let digest = state
        .execute_project_action(&project_id, "approve")
        .unwrap();

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        CodexRuntimeCommand::JsonRpcResponse { response } => {
            assert_eq!(response.request_id, json!("req-project-approve"));
            assert_eq!(
                response.payload,
                CodexJsonRpcResponsePayload::Result(json!({ "decision": "accept" }))
            );
        }
        _ => panic!("expected approval response"),
    }

    assert_eq!(digest.current_status, "Agent is working.");
    assert!(
        !digest
            .proposed_actions
            .iter()
            .any(|action| action.id == "approve")
    );

    fs::remove_dir_all(root).unwrap();
}

// Tests that project action keep iterating dispatches a follow up prompt.
#[test]
fn project_action_keep_iterating_dispatches_a_follow_up_prompt() {
    let state = test_app_state();
    let repo_root = std::env::temp_dir().join(format!("termal-project-iterate-{}", Uuid::new_v4()));
    fs::create_dir_all(repo_root.join("src")).unwrap();
    fs::write(
        repo_root.join("src/lib.rs"),
        "pub fn value() -> u32 { 1 }\n",
    )
    .unwrap();

    run_git_test_command(&repo_root, &["init"]);
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);
    run_git_test_command(&repo_root, &["add", "."]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);

    fs::write(
        repo_root.join("src/lib.rs"),
        "pub fn value() -> u32 { 2 }\n",
    )
    .unwrap();

    let project_id = create_test_project(&state, &repo_root, "Iterate Project");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &repo_root);
    let (runtime, input_rx) = test_codex_runtime_handle("project-iterate");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }

    let digest = state
        .execute_project_action(&project_id, "keep-iterating")
        .unwrap();

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        CodexRuntimeCommand::Prompt {
            session_id: runtime_session_id,
            command,
        } => {
            assert_eq!(runtime_session_id, session_id);
            assert_eq!(
                command.prompt,
                ProjectActionId::KeepIterating.prompt().unwrap()
            );
        }
        _ => panic!("expected prompt dispatch"),
    }

    assert_eq!(digest.current_status, "Agent is working.");
    assert_eq!(
        digest
            .proposed_actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>(),
        vec!["stop", "review-in-termal"]
    );

    fs::remove_dir_all(repo_root).unwrap();
}

// Tests that Telegram command parser supports suffixes and aliases.
#[test]
fn telegram_command_parser_supports_suffixes_and_aliases() {
    let parsed =
        parse_telegram_command("/commit@termal_bot   now please").expect("command should parse");
    assert_eq!(
        parsed.command,
        TelegramIncomingCommand::Action(ProjectActionId::AskAgentToCommit)
    );
    assert_eq!(parsed.args, "now please");

    let parsed = parse_telegram_command("/status").expect("status should parse");
    assert_eq!(parsed.command, TelegramIncomingCommand::Status);
}

// Tests that Telegram command parser rejects unknown slash commands.
#[test]
fn telegram_command_parser_rejects_unknown_slash_commands() {
    assert!(parse_telegram_command("/unknown").is_none());
}

// Tests that Telegram digest renderer includes actions and public link.
#[test]
fn telegram_digest_renderer_includes_actions_and_public_link() {
    let digest = ProjectDigestResponse {
        project_id: "project-1".to_owned(),
        headline: "termal".to_owned(),
        done_summary: "Updated the digest API.".to_owned(),
        current_status: "Changes are ready for review.".to_owned(),
        primary_session_id: Some("session-1".to_owned()),
        proposed_actions: vec![
            ProjectActionId::ReviewInTermal.into_digest_action(),
            ProjectActionId::AskAgentToCommit.into_digest_action(),
        ],
        deep_link: Some("/?projectId=project-1&sessionId=session-1".to_owned()),
        source_message_ids: vec!["message-1".to_owned()],
    };

    let rendered = render_telegram_digest(&digest, Some("https://termal.local"));
    assert!(rendered.contains("Project: termal"));
    assert!(rendered.contains("Next: Review in TermAl, Ask Agent to Commit"));
    assert!(
        rendered.contains("Open: https://termal.local/?projectId=project-1&sessionId=session-1")
    );

    let keyboard = build_telegram_digest_keyboard(&digest).expect("keyboard should exist");
    assert_eq!(keyboard.inline_keyboard.len(), 1);
    assert_eq!(
        keyboard.inline_keyboard[0][0].callback_data,
        "review-in-termal"
    );
    assert_eq!(
        keyboard.inline_keyboard[0][1].callback_data,
        "ask-agent-to-commit"
    );
}

fn persisted_state_load_error_after_mutation<F>(inner: StateInner, mutate: F) -> String
where
    F: FnOnce(&mut Value),
{
    let path =
        std::env::temp_dir().join(format!("termal-state-load-error-{}.json", Uuid::new_v4()));
    persist_state(&path, &inner).expect("persisted state should be written");

    let mut encoded: Value = serde_json::from_slice(&fs::read(&path).unwrap())
        .expect("persisted state should deserialize");
    mutate(&mut encoded);
    fs::write(&path, serde_json::to_vec(&encoded).unwrap()).expect("persisted state should update");

    let err = match load_state(&path) {
        Ok(_) => panic!("mutated persisted state should fail to load"),
        Err(err) => err,
    };
    let _ = fs::remove_file(path);
    format!("{err:#}")
}

// Tests that persisted state normalizes legacy local verbatim paths.
#[cfg(windows)]
#[test]
fn persisted_state_normalizes_legacy_local_verbatim_paths() {
    let project_root =
        std::env::temp_dir().join(format!("termal-legacy-verbatim-path-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let legacy_root = format!(r"\\?\{normalized_root}");
    let path = std::env::temp_dir().join(format!(
        "termal-legacy-verbatim-state-{}.json",
        Uuid::new_v4()
    ));

    let mut inner = StateInner::new();
    let project = inner.create_project(None, normalized_root.clone(), default_local_remote_id());
    inner.create_session(
        Agent::Claude,
        Some("Claude".to_owned()),
        normalized_root.clone(),
        Some(project.id),
        None,
    );
    persist_state(&path, &inner).expect("persisted state should be written");

    let mut encoded: Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).expect("state should deserialize");
    encoded["projects"][0]["rootPath"] = Value::String(legacy_root.clone());
    encoded["sessions"][0]["session"]["workdir"] = Value::String(legacy_root);
    fs::write(&path, serde_json::to_vec(&encoded).unwrap()).expect("persisted state should update");

    let loaded = load_state(&path)
        .expect("persisted state should load")
        .expect("persisted state should exist");
    assert_eq!(loaded.projects[0].root_path, normalized_root);
    assert_eq!(loaded.sessions[0].session.workdir, normalized_root);

    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(project_root);
}

// Tests that persisted state normalizes legacy workspace layout paths.
#[cfg(windows)]
#[test]
fn persisted_state_normalizes_legacy_workspace_layout_paths() {
    let project_root =
        std::env::temp_dir().join(format!("termal-layout-verbatim-path-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let legacy_root = format!(r"\\?\{normalized_root}");
    let normalized_file = format!(r"{normalized_root}\src\main.rs");
    let legacy_file = format!(r"\\?\{normalized_file}");
    let path = std::env::temp_dir().join(format!(
        "termal-layout-verbatim-state-{}.json",
        Uuid::new_v4()
    ));

    let mut inner = StateInner::new();
    inner.workspace_layouts.insert(
        "workspace-1".to_owned(),
        WorkspaceLayoutDocument {
            id: "workspace-1".to_owned(),
            revision: 1,
            updated_at: "2026-04-01 12:00:00".to_owned(),
            control_panel_side: WorkspaceControlPanelSide::Left,
            theme_id: None,
            style_id: None,
            font_size_px: None,
            editor_font_size_px: None,
            density_percent: None,
            workspace: json!({
                "root": {
                    "type": "pane",
                    "paneId": "pane-a"
                },
                "panes": [{
                    "id": "pane-a",
                    "tabs": [
                        {
                            "id": "tab-files",
                            "kind": "filesystem",
                            "rootPath": legacy_root,
                            "originSessionId": serde_json::Value::Null
                        },
                        {
                            "id": "tab-git",
                            "kind": "gitStatus",
                            "workdir": format!(r"\\?\{normalized_root}"),
                            "originSessionId": serde_json::Value::Null
                        },
                        {
                            "id": "tab-debug",
                            "kind": "instructionDebugger",
                            "workdir": format!(r"\\?\{normalized_root}"),
                            "originSessionId": serde_json::Value::Null
                        },
                        {
                            "id": "tab-source",
                            "kind": "source",
                            "path": legacy_file,
                            "originSessionId": serde_json::Value::Null
                        },
                        {
                            "id": "tab-diff",
                            "kind": "diffPreview",
                            "changeType": "edit",
                            "diff": "-before\n+after",
                            "diffMessageId": "message-1",
                            "filePath": format!(r"\\?\{normalized_file}"),
                            "originSessionId": serde_json::Value::Null,
                            "summary": "Updated file"
                        }
                    ],
                    "activeTabId": "tab-files",
                    "activeSessionId": serde_json::Value::Null,
                    "viewMode": "filesystem",
                    "lastSessionViewMode": "session",
                    "sourcePath": format!(r"\\?\{normalized_file}")
                }],
                "activePaneId": "pane-a"
            }),
        },
    );
    persist_state(&path, &inner).expect("persisted state should be written");

    let loaded = load_state(&path)
        .expect("persisted state should load")
        .expect("persisted state should exist");
    let layout = loaded
        .workspace_layouts
        .get("workspace-1")
        .expect("workspace layout should load");
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/sourcePath")
            .and_then(Value::as_str),
        Some(normalized_file.as_str())
    );
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/tabs/0/rootPath")
            .and_then(Value::as_str),
        Some(normalized_root.as_str())
    );
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/tabs/1/workdir")
            .and_then(Value::as_str),
        Some(normalized_root.as_str())
    );
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/tabs/2/workdir")
            .and_then(Value::as_str),
        Some(normalized_root.as_str())
    );
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/tabs/3/path")
            .and_then(Value::as_str),
        Some(normalized_file.as_str())
    );
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/tabs/4/filePath")
            .and_then(Value::as_str),
        Some(normalized_file.as_str())
    );

    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(project_root);
}

// Tests that app state bootstrap normalizes legacy local verbatim working directory.
#[test]
fn app_state_bootstrap_normalizes_legacy_local_verbatim_workdir() {
    let project_root =
        std::env::temp_dir().join(format!("termal-bootstrap-verbatim-path-{}", Uuid::new_v4()));
    let state_root =
        std::env::temp_dir().join(format!("termal-bootstrap-state-root-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&state_root).expect("state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let legacy_root = format!(r"\\?\{normalized_root}");
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");

    let state = AppState::new_with_paths(
        legacy_root,
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");

    assert_eq!(state.default_workdir, normalized_root);
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.projects.len(), 1);
    assert_eq!(inner.projects[0].root_path, normalized_root);
    let bootstrapped_sessions = inner
        .sessions
        .iter()
        .filter(|record| matches!(record.session.name.as_str(), "Codex Live" | "Claude Live"))
        .collect::<Vec<_>>();
    assert_eq!(bootstrapped_sessions.len(), 2);
    assert!(
        bootstrapped_sessions
            .iter()
            .all(|record| record.session.workdir == normalized_root)
    );
    drop(inner);
    drop(state);

    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(project_root);
}

// Tests that persisted state preserves significant local path spaces.
#[cfg(not(windows))]
#[test]
fn persisted_state_preserves_significant_local_path_spaces() {
    let project_root =
        std::env::temp_dir().join(format!("termal-significant-path-space-{} ", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let path = std::env::temp_dir().join(format!(
        "termal-significant-path-space-state-{}.json",
        Uuid::new_v4()
    ));

    assert!(normalized_root.ends_with(' '));

    let mut inner = StateInner::new();
    let project = inner.create_project(None, normalized_root.clone(), default_local_remote_id());
    inner.create_session(
        Agent::Claude,
        Some("Claude".to_owned()),
        normalized_root.clone(),
        Some(project.id),
        None,
    );
    persist_state(&path, &inner).expect("persisted state should be written");

    let loaded = load_state(&path)
        .expect("persisted state should load")
        .expect("persisted state should exist");
    assert_eq!(loaded.projects[0].root_path, normalized_root);
    assert_eq!(loaded.sessions[0].session.workdir, normalized_root);

    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(project_root);
}

// Tests that persisted state requires projects.
#[test]
fn persisted_state_requires_projects() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Codex,
        Some("Migrated".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        encoded
            .as_object_mut()
            .expect("persisted state should be an object")
            .remove("projects");
    });

    assert!(
        err_text.contains("missing field `projects`"),
        "unexpected load_state error: {err_text}"
    );
}

// Tests that persisted state requires next project number.
#[test]
fn persisted_state_requires_next_project_number() {
    let inner = StateInner::new();

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        encoded
            .as_object_mut()
            .expect("persisted state should be an object")
            .remove("nextProjectNumber");
    });

    assert!(
        err_text.contains("missing field `nextProjectNumber`"),
        "unexpected load_state error: {err_text}"
    );
}

// Tests that persisted state requires project remote ID.
#[test]
fn persisted_state_requires_project_remote_id() {
    let mut inner = StateInner::new();
    inner.create_project(None, "/tmp".to_owned(), default_local_remote_id());

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        encoded["projects"]
            .as_array_mut()
            .expect("persisted projects should be an array")[0]
            .as_object_mut()
            .expect("persisted project should be an object")
            .remove("remoteId");
    });

    assert!(
        err_text.contains("missing field `remoteId`"),
        "unexpected load_state error: {err_text}"
    );
}

// Tests that persisted state requires valid remotes.
#[test]
fn persisted_state_requires_valid_remotes() {
    let inner = StateInner::new();

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        encoded["preferences"]["remotes"] = json!([
            {
                "id": "local",
                "name": "Local",
                "transport": "local",
                "enabled": true
            },
            {
                "id": "ssh-1",
                "name": "pop-os",
                "transport": "ssh",
                "enabled": true,
                "host": "pop-os.local",
                "port": 22,
                "user": "greg"
            },
            {
                "id": "ssh-1",
                "name": "backup",
                "transport": "ssh",
                "enabled": true,
                "host": "backup.local",
                "port": 22,
                "user": "greg"
            }
        ]);
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("duplicate remote id `ssh-1`"),
        "unexpected load_state error: {err_text}"
    );
}

// Tests that persisted state requires cursor mode.
#[test]
fn persisted_state_requires_cursor_mode() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Cursor,
        Some("Cursor".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        let session = encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]["session"]
            .as_object_mut()
            .expect("persisted session should be an object");
        session.remove("cursorMode");
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("missing cursorMode"),
        "unexpected load_state error: {err_text}"
    );
}

// Tests that persisted state requires Claude settings.
#[test]
fn persisted_state_requires_claude_settings() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Claude,
        Some("Claude".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        let session = encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]["session"]
            .as_object_mut()
            .expect("persisted session should be an object");
        session.remove("claudeApprovalMode");
        session.remove("claudeEffort");
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("missing claudeApprovalMode"),
        "unexpected load_state error: {err_text}"
    );
}

// Tests that persisted state requires Gemini approval mode.
#[test]
fn persisted_state_requires_gemini_approval_mode() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Gemini,
        Some("Gemini".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        let session = encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]["session"]
            .as_object_mut()
            .expect("persisted session should be an object");
        session.remove("geminiApprovalMode");
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("missing geminiApprovalMode"),
        "unexpected load_state error: {err_text}"
    );
}

// Tests that persisted state requires Codex prompt fields.
#[test]
fn persisted_state_requires_codex_prompt_fields() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Codex,
        Some("Codex".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        let session = encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]["session"]
            .as_object_mut()
            .expect("persisted session should be an object");
        session.remove("approvalPolicy");
        session.remove("reasoningEffort");
        session.remove("sandboxMode");
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("missing approvalPolicy"),
        "unexpected load_state error: {err_text}"
    );
}

// Tests that persisted state requires Codex thread state for live threads.
#[test]
fn persisted_state_requires_codex_thread_state_for_live_threads() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Codex,
        Some("Codex".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        let entry = encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]
            .as_object_mut()
            .expect("persisted session record should be an object");
        entry.insert(
            "externalSessionId".to_owned(),
            Value::String("thread-live".to_owned()),
        );
        let session = entry["session"]
            .as_object_mut()
            .expect("persisted session should be an object");
        session.insert(
            "externalSessionId".to_owned(),
            Value::String("thread-live".to_owned()),
        );
        session.remove("codexThreadState");
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("missing codexThreadState"),
        "unexpected load_state error: {err_text}"
    );
}

// Tests that persisted state requires queued prompt source.
#[test]
fn persisted_state_requires_queued_prompt_source() {
    let path = std::env::temp_dir().join(format!(
        "termal-queued-prompt-source-required-{}",
        Uuid::new_v4()
    ));
    let mut inner = StateInner::new();
    let record = inner.create_session(
        Agent::Codex,
        Some("Queued".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let session_id = record.session.id.clone();
    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    queue_prompt_on_record(
        &mut inner.sessions[index],
        PendingPrompt {
            attachments: Vec::new(),
            id: "queued-prompt-1".to_owned(),
            timestamp: stamp_now(),
            text: "queued prompt".to_owned(),
            expanded_text: None,
        },
        Vec::new(),
    );
    persist_state(&path, &inner).unwrap();

    let mut encoded: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let sessions = encoded["sessions"]
        .as_array_mut()
        .expect("persisted sessions should be an array");
    let queued_prompts = sessions[0]["queuedPrompts"]
        .as_array_mut()
        .expect("persisted queued prompts should be an array");
    queued_prompts[0]
        .as_object_mut()
        .expect("queued prompt should be an object")
        .remove("source");
    fs::write(&path, serde_json::to_vec(&encoded).unwrap()).unwrap();

    let err = match load_state(&path) {
        Ok(_) => panic!("persisted state without queued prompt source should fail"),
        Err(err) => err,
    };
    let err_text = format!("{err:#}");
    assert!(
        err_text.contains("missing field `source`"),
        "unexpected load_state error: {err_text}"
    );

    let _ = fs::remove_file(path);
}

// Tests that create orchestrator instance route uses template project when request project ID is empty.
#[tokio::test]
async fn create_orchestrator_instance_route_uses_template_project_when_request_project_id_is_empty()
{
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-route-empty-project-id-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("route fallback project root should exist");
    let project_id = create_test_project(&state, &project_root, "Route Fallback Project");
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;
    let template_id = template.id.clone();
    let template_session_count = template.sessions.len();

    let app = app_router(state);
    let (status, response): (StatusCode, CreateOrchestratorInstanceResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/orchestrators")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "templateId": template_id,
                    "projectId": "",
                }))
                .expect("request body should serialize"),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(response.orchestrator.template_id, template.id);
    assert_eq!(response.orchestrator.project_id, project_id);
    assert_eq!(
        response
            .orchestrator
            .template_snapshot
            .project_id
            .as_deref(),
        Some(response.orchestrator.project_id.as_str())
    );
    assert_eq!(
        response.orchestrator.session_instances.len(),
        template_session_count
    );
    let _ = fs::remove_dir_all(project_root);
}

// Tests that orchestrator lifecycle routes update state and stop active sessions.
#[tokio::test]
async fn orchestrator_lifecycle_routes_update_state_and_stop_active_sessions() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-lifecycle-route-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("route project root should exist");
    let project_id = create_test_project(&state, &project_root, "Route Orchestrator Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let (runtime, _input_rx) = test_claude_runtime_handle("route-orchestrator-stop");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        inner.sessions[planner_index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[planner_index].session.status = SessionStatus::Active;
        inner.sessions[planner_index].active_turn_start_message_count =
            Some(inner.sessions[planner_index].session.messages.len());
        inner.sessions[planner_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-orchestrator-stop-follow-up".to_owned(),
                    timestamp: stamp_now(),
                    text: "queued orchestrator follow-up".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[planner_index]);
    }

    let app = app_router(state.clone());
    let (pause_status, pause_response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/orchestrators/{instance_id}/pause"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(pause_status, StatusCode::OK);
    let paused = pause_response
        .orchestrators
        .iter()
        .find(|instance| instance.id == instance_id)
        .expect("paused orchestrator should be present");
    assert_eq!(paused.status, OrchestratorInstanceStatus::Paused);

    let (resume_status, resume_response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/orchestrators/{instance_id}/resume"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resume_status, StatusCode::OK);
    let resumed = resume_response
        .orchestrators
        .iter()
        .find(|instance| instance.id == instance_id)
        .expect("resumed orchestrator should be present");
    assert_eq!(resumed.status, OrchestratorInstanceStatus::Running);

    let (stop_status, stop_response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/orchestrators/{instance_id}/stop"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(stop_status, StatusCode::OK);
    let stopped = stop_response
        .orchestrators
        .iter()
        .find(|instance| instance.id == instance_id)
        .expect("stopped orchestrator should be present");
    assert_eq!(stopped.status, OrchestratorInstanceStatus::Stopped);
    assert!(stopped.pending_transitions.is_empty());
    assert!(stopped.completed_at.is_some());

    let planner_session = stop_response
        .sessions
        .iter()
        .find(|session| session.id == planner_session_id)
        .expect("planner session should still be present");
    assert_eq!(planner_session.status, SessionStatus::Idle);
    assert!(planner_session.pending_prompts.is_empty());

    let inner = state.inner.lock().expect("state mutex poisoned");
    let planner_record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == planner_session_id)
        .expect("planner session should still exist");
    assert_eq!(planner_record.session.status, SessionStatus::Idle);
    assert!(matches!(planner_record.runtime, SessionRuntime::None));
    assert!(planner_record.queued_prompts.is_empty());
    assert!(planner_record.session.pending_prompts.is_empty());
    drop(inner);
    assert!(planner_session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn stopped by user."
    )));

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}
// Tests that orchestrator stop route preserves running state when a child stop fails.
#[tokio::test]
async fn orchestrator_stop_route_preserves_running_state_when_a_child_stop_fails() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-failure-route-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("route project root should exist");
    let project_id = create_test_project(&state, &project_root, "Route Orchestrator Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let reviewer_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "reviewer")
        .expect("reviewer session should be mapped")
        .session_id
        .clone();
    let failing_process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (planner_input_tx, _planner_input_rx) = mpsc::channel();
    let planner_runtime = ClaudeRuntimeHandle {
        runtime_id: "route-orchestrator-stop-fail".to_owned(),
        input_tx: planner_input_tx,
        process: failing_process.clone(),
    };
    let (reviewer_runtime, _reviewer_input_rx) =
        test_claude_runtime_handle("route-orchestrator-stop-reviewer");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        for (session_id, runtime) in [
            (
                planner_session_id.clone(),
                SessionRuntime::Claude(planner_runtime),
            ),
            (
                reviewer_session_id.clone(),
                SessionRuntime::Claude(reviewer_runtime),
            ),
        ] {
            let index = inner
                .find_session_index(&session_id)
                .expect("orchestrator session should exist");
            inner.sessions[index].runtime = runtime;
            inner.sessions[index].session.status = SessionStatus::Active;
            inner.sessions[index].active_turn_start_message_count =
                Some(inner.sessions[index].session.messages.len());
        }
    }

    let app = app_router(state.clone());
    let failure_guard = force_test_kill_child_process_failure(&failing_process, "Claude");
    let response = request_response(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/orchestrators/{instance_id}/stop"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body should read");
    let error: Value = serde_json::from_slice(&body).expect("error response should parse");
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|message| message.contains("failed to stop session `"))
    );
    drop(failure_guard);

    let snapshot = state.snapshot();
    let instance = snapshot
        .orchestrators
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("orchestrator should still be present");
    assert_eq!(instance.status, OrchestratorInstanceStatus::Running);
    assert!(instance.completed_at.is_none());

    let planner_session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == planner_session_id)
        .expect("planner session should still be present");
    assert_eq!(planner_session.status, SessionStatus::Active);

    let reviewer_session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == reviewer_session_id)
        .expect("reviewer session should still be present");
    assert_eq!(reviewer_session.status, SessionStatus::Idle);
    assert!(reviewer_session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn stopped by user."
    )));

    let reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    let reloaded_instance = reloaded_inner
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("persisted orchestrator should still exist");
    assert_eq!(
        reloaded_instance.status,
        OrchestratorInstanceStatus::Running
    );
    assert!(reloaded_instance.completed_at.is_none());
    let reloaded_reviewer = reloaded_inner
        .sessions
        .iter()
        .find(|record| record.session.id == reviewer_session_id)
        .expect("persisted reviewer session should exist");
    assert_eq!(reloaded_reviewer.session.status, SessionStatus::Idle);

    let _ = failing_process.kill();
    let _ = failing_process.wait();
    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that aborted stop cleanup preserves child work when child stop persist fails.
#[test]
fn aborted_stop_cleanup_preserves_child_work_when_child_stop_persist_fails() {
    let mut state = test_app_state();
    let original_persistence_path = state.persistence_path.clone();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-cleanup-{}",
        Uuid::new_v4()
    ));
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-cleanup-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");
    let project_id = create_test_project(&state, &project_root, "Persist Failure Cleanup");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (builder_runtime, _builder_input_rx) =
        test_claude_runtime_handle("orchestrator-stop-persist-failure-builder");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let instance_index = inner
            .orchestrator_instances
            .iter()
            .position(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        inner.sessions[builder_index].runtime = SessionRuntime::Claude(builder_runtime);
        inner.sessions[builder_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index].active_turn_start_message_count =
            Some(inner.sessions[builder_index].session.messages.len());
        inner.sessions[builder_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-persist-failure-cleanup-builder".to_owned(),
                    timestamp: stamp_now(),
                    text: "builder work that should survive aborted cleanup".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[builder_index]);
        let completion_revision = inner.revision.saturating_add(1);
        inner.orchestrator_instances[instance_index]
            .pending_transitions
            .push(PendingTransition {
                id: format!("pending-transition-{}", Uuid::new_v4()),
                transition_id: "planner-to-builder".to_owned(),
                source_session_id: planner_session_id.clone(),
                destination_session_id: builder_session_id.clone(),
                completion_revision,
                rendered_prompt: "builder pending work should survive aborted cleanup".to_owned(),
                created_at: stamp_orchestrator_template_now(),
            });
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .begin_orchestrator_stop(&instance_id)
        .expect("stop should be marked in progress");
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    // Force synchronous persistence so the error propagates instead of
    // being swallowed by the background persist thread.
    state.persist_tx = mpsc::channel().0;

    let error = state
        .stop_session_with_options(
            &builder_session_id,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: false,
                orchestrator_stop_instance_id: Some(instance_id.clone()),
            },
        )
        .err()
        .expect("persist failures should abort child stop persistence");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to persist session state"));

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should still exist");
        assert!(instance.stopped_session_ids_during_stop.is_empty());
    }
    assert!(
        state
            .stopping_orchestrator_session_ids
            .lock()
            .expect("orchestrator stop session mutex poisoned")
            .get(&instance_id)
            .is_some_and(|session_ids| session_ids.is_empty())
    );

    state.persistence_path = original_persistence_path.clone();
    state
        .prune_pending_transitions_for_stopped_orchestrator_sessions(&instance_id)
        .expect("aborted cleanup should preserve work for uncommitted child stops");

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should still exist");
        assert_eq!(instance.status, OrchestratorInstanceStatus::Running);
        assert!(!instance.stop_in_progress);
        assert!(instance.active_session_ids_during_stop.is_none());
        assert!(instance.stopped_session_ids_during_stop.is_empty());
        assert_eq!(instance.pending_transitions.len(), 1);
        assert_eq!(
            instance.pending_transitions[0].destination_session_id,
            builder_session_id
        );
        let builder = inner
            .sessions
            .iter()
            .find(|record| record.session.id == builder_session_id)
            .expect("builder session should still exist");
        assert_eq!(builder.queued_prompts.len(), 1);
        assert_eq!(builder.session.pending_prompts.len(), 1);
    }

    let reloaded_inner = load_state(original_persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    let reloaded_instance = reloaded_inner
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("persisted orchestrator should still exist");
    assert_eq!(
        reloaded_instance.status,
        OrchestratorInstanceStatus::Running
    );
    assert!(!reloaded_instance.stop_in_progress);
    assert!(reloaded_instance.active_session_ids_during_stop.is_none());
    assert!(reloaded_instance.stopped_session_ids_during_stop.is_empty());
    assert_eq!(reloaded_instance.pending_transitions.len(), 1);
    assert_eq!(
        reloaded_instance.pending_transitions[0].destination_session_id,
        builder_session_id
    );
    let reloaded_builder = reloaded_inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("persisted builder session should still exist");
    assert_eq!(reloaded_builder.queued_prompts.len(), 1);
    assert_eq!(reloaded_builder.session.pending_prompts.len(), 1);

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(original_persistence_path.as_path());
    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Tests that aborted stop resume does not redispatch child after child stop persist fails.
#[test]
fn aborted_stop_resume_does_not_redispatch_child_after_child_stop_persist_fails() {
    let mut state = test_app_state();
    let original_persistence_path = state.persistence_path.clone();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-resume-{}",
        Uuid::new_v4()
    ));
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-resume-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");
    let project_id = create_test_project(&state, &project_root, "Persist Failure Resume");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (builder_runtime, _builder_input_rx) =
        test_claude_runtime_handle("orchestrator-stop-persist-failure-resume-builder");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let instance_index = inner
            .orchestrator_instances
            .iter()
            .position(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        inner.sessions[builder_index].runtime = SessionRuntime::Claude(builder_runtime);
        inner.sessions[builder_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index].active_turn_start_message_count =
            Some(inner.sessions[builder_index].session.messages.len());
        let completion_revision = inner.revision.saturating_add(1);
        inner.orchestrator_instances[instance_index]
            .pending_transitions
            .push(PendingTransition {
                id: format!("pending-transition-{}", Uuid::new_v4()),
                transition_id: "planner-to-builder".to_owned(),
                source_session_id: planner_session_id.clone(),
                destination_session_id: builder_session_id.clone(),
                completion_revision,
                rendered_prompt: "builder pending work should remain pending after resume"
                    .to_owned(),
                created_at: stamp_orchestrator_template_now(),
            });
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .begin_orchestrator_stop(&instance_id)
        .expect("stop should be marked in progress");
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    // Force synchronous persistence so the error propagates instead of
    // being swallowed by the background persist thread.
    state.persist_tx = mpsc::channel().0;

    let error = state
        .stop_session_with_options(
            &builder_session_id,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: false,
                orchestrator_stop_instance_id: Some(instance_id.clone()),
            },
        )
        .err()
        .expect("persist failures should abort child stop persistence");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to persist session state"));

    state.persistence_path = original_persistence_path.clone();
    state
        .prune_pending_transitions_for_stopped_orchestrator_sessions(&instance_id)
        .expect("aborted cleanup should preserve pending child work");
    state.finish_orchestrator_stop(&instance_id);
    state
        .resume_pending_orchestrator_transitions()
        .expect("aborted stop resume should succeed without redispatching the blocked child");

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should still exist");
        assert_eq!(instance.status, OrchestratorInstanceStatus::Running);
        assert_eq!(instance.pending_transitions.len(), 1);
        assert_eq!(
            instance.pending_transitions[0].destination_session_id,
            builder_session_id
        );
        let builder = inner
            .sessions
            .iter()
            .find(|record| record.session.id == builder_session_id)
            .expect("builder session should still exist");
        assert_eq!(builder.session.status, SessionStatus::Idle);
        assert!(matches!(builder.runtime, SessionRuntime::None));
        assert!(builder.orchestrator_auto_dispatch_blocked);
        assert!(builder.queued_prompts.is_empty());
        assert!(builder.session.pending_prompts.is_empty());
    }

    let reloaded_inner = load_state(original_persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    let reloaded_instance = reloaded_inner
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("persisted orchestrator should still exist");
    assert_eq!(
        reloaded_instance.status,
        OrchestratorInstanceStatus::Running
    );
    assert_eq!(reloaded_instance.pending_transitions.len(), 1);
    assert_eq!(
        reloaded_instance.pending_transitions[0].destination_session_id,
        builder_session_id
    );
    let reloaded_builder = reloaded_inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("persisted builder session should still exist");
    assert!(reloaded_builder.orchestrator_auto_dispatch_blocked);

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(original_persistence_path.as_path());
    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Tests that aborted stop restart does not redispatch child after child stop persist fails.
#[test]
fn aborted_stop_restart_does_not_redispatch_child_after_child_stop_persist_fails() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-restart-{}",
        Uuid::new_v4()
    ));
    let state_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-restart-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&state_root).expect("state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");
    let failing_persistence_path = state_root.join("persist-failure");
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    let mut state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    let project_id = create_test_project(&state, &project_root, "Persist Failure Restart");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (builder_runtime, _builder_input_rx) =
        test_claude_runtime_handle("orchestrator-stop-persist-failure-restart-builder");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let instance_index = inner
            .orchestrator_instances
            .iter()
            .position(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        inner.sessions[builder_index].runtime = SessionRuntime::Claude(builder_runtime);
        inner.sessions[builder_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index].active_turn_start_message_count =
            Some(inner.sessions[builder_index].session.messages.len());
        let completion_revision = inner.revision.saturating_add(1);
        inner.orchestrator_instances[instance_index]
            .pending_transitions
            .push(PendingTransition {
                id: format!("pending-transition-{}", Uuid::new_v4()),
                transition_id: "planner-to-builder".to_owned(),
                source_session_id: planner_session_id.clone(),
                destination_session_id: builder_session_id.clone(),
                completion_revision,
                rendered_prompt: "builder pending work should remain pending after restart"
                    .to_owned(),
                created_at: stamp_orchestrator_template_now(),
            });
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .begin_orchestrator_stop(&instance_id)
        .expect("stop should be marked in progress");
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    // Force synchronous persistence so the error propagates instead of
    // being swallowed by the background persist thread.
    state.persist_tx = mpsc::channel().0;

    let error = state
        .stop_session_with_options(
            &builder_session_id,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: false,
                orchestrator_stop_instance_id: Some(instance_id.clone()),
            },
        )
        .err()
        .expect("persist failures should abort child stop persistence");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to persist session state"));

    state.persistence_path = Arc::new(persistence_path.clone());
    state
        .prune_pending_transitions_for_stopped_orchestrator_sessions(&instance_id)
        .expect("aborted cleanup should preserve pending child work");
    state.finish_orchestrator_stop(&instance_id);
    drop(state);

    let restarted = AppState::new_with_paths(
        normalized_root,
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should restart");

    {
        let inner = restarted.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should still exist after restart");
        assert_eq!(instance.status, OrchestratorInstanceStatus::Running);
        assert_eq!(instance.pending_transitions.len(), 1);
        assert_eq!(
            instance.pending_transitions[0].destination_session_id,
            builder_session_id
        );
        let builder = inner
            .sessions
            .iter()
            .find(|record| record.session.id == builder_session_id)
            .expect("builder session should still exist after restart");
        assert_eq!(builder.session.status, SessionStatus::Idle);
        assert!(matches!(builder.runtime, SessionRuntime::None));
        assert!(builder.orchestrator_auto_dispatch_blocked);
        assert!(builder.queued_prompts.is_empty());
        assert!(builder.session.pending_prompts.is_empty());
    }

    drop(restarted);

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(state_root);
}

// Tests that aborted stop restart does not dispatch orphaned child queue after child stop persist fails.
#[test]
fn aborted_stop_restart_does_not_dispatch_orphaned_child_queue_after_child_stop_persist_fails() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-restart-queued-{}",
        Uuid::new_v4()
    ));
    let state_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-restart-queued-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&state_root).expect("state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");
    let failing_persistence_path = state_root.join("persist-failure");
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    let mut state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    let project_id = create_test_project(&state, &project_root, "Persist Failure Restart Queue");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (builder_runtime, _builder_input_rx) =
        test_claude_runtime_handle("orchestrator-stop-persist-failure-restart-queued-builder");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        inner.sessions[builder_index].runtime = SessionRuntime::Claude(builder_runtime);
        inner.sessions[builder_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index].active_turn_start_message_count =
            Some(inner.sessions[builder_index].session.messages.len());
        inner.sessions[builder_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-persist-failure-restart-builder".to_owned(),
                    timestamp: stamp_now(),
                    text: "builder queued work should remain parked after restart".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[builder_index]);
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .begin_orchestrator_stop(&instance_id)
        .expect("stop should be marked in progress");
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    // Force synchronous persistence so the error propagates instead of
    // being swallowed by the background persist thread.
    state.persist_tx = mpsc::channel().0;

    let error = state
        .stop_session_with_options(
            &builder_session_id,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: false,
                orchestrator_stop_instance_id: Some(instance_id.clone()),
            },
        )
        .err()
        .expect("persist failures should abort child stop persistence");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to persist session state"));

    state.persistence_path = Arc::new(persistence_path.clone());
    state
        .prune_pending_transitions_for_stopped_orchestrator_sessions(&instance_id)
        .expect("aborted cleanup should preserve queued child work");
    state.finish_orchestrator_stop(&instance_id);
    drop(state);

    let restarted = AppState::new_with_paths(
        normalized_root,
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should restart");

    {
        let inner = restarted.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should still exist after restart");
        assert_eq!(instance.status, OrchestratorInstanceStatus::Running);
        assert!(instance.pending_transitions.is_empty());
        let builder = inner
            .sessions
            .iter()
            .find(|record| record.session.id == builder_session_id)
            .expect("builder session should still exist after restart");
        assert_eq!(builder.session.status, SessionStatus::Idle);
        assert!(matches!(builder.runtime, SessionRuntime::None));
        assert!(builder.orchestrator_auto_dispatch_blocked);
        assert_eq!(builder.queued_prompts.len(), 1);
        assert_eq!(builder.session.pending_prompts.len(), 1);
        assert_eq!(
            builder.queued_prompts[0].pending_prompt.text,
            "builder queued work should remain parked after restart"
        );
    }

    drop(restarted);

    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(project_root);
}

// Tests that blocked session manual recovery dispatch prioritizes user prompt after restart.
#[test]
fn blocked_session_manual_recovery_dispatch_prioritizes_user_prompt_after_restart() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-manual-recovery-{}",
        Uuid::new_v4()
    ));
    let state_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-manual-recovery-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&state_root).expect("state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");
    let failing_persistence_path = state_root.join("persist-failure");
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    let mut state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    let project_id = create_test_project(&state, &project_root, "Manual Recovery Ordering");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let reviewer_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "reviewer")
        .expect("reviewer session should be mapped")
        .session_id
        .clone();
    let (reviewer_runtime, _reviewer_input_rx) =
        test_claude_runtime_handle("orchestrator-stop-persist-failure-manual-recovery-reviewer");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let reviewer_index = inner
            .find_session_index(&reviewer_session_id)
            .expect("reviewer session should exist");
        inner.sessions[reviewer_index].runtime = SessionRuntime::Claude(reviewer_runtime);
        inner.sessions[reviewer_index].session.status = SessionStatus::Active;
        inner.sessions[reviewer_index].active_turn_start_message_count =
            Some(inner.sessions[reviewer_index].session.messages.len());
        inner.sessions[reviewer_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-persist-failure-manual-recovery-reviewer".to_owned(),
                    timestamp: stamp_now(),
                    text: "reviewer queued work should stay behind the user prompt".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[reviewer_index]);
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .begin_orchestrator_stop(&instance_id)
        .expect("stop should be marked in progress");
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    state.persist_tx = mpsc::channel().0;

    let error = state
        .stop_session_with_options(
            &reviewer_session_id,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: false,
                orchestrator_stop_instance_id: Some(instance_id.clone()),
            },
        )
        .err()
        .expect("persist failures should abort child stop persistence");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to persist session state"));

    state.persistence_path = Arc::new(persistence_path.clone());
    state
        .prune_pending_transitions_for_stopped_orchestrator_sessions(&instance_id)
        .expect("aborted cleanup should preserve queued child work");
    state.finish_orchestrator_stop(&instance_id);
    drop(state);

    let restarted = AppState::new_with_paths(
        normalized_root,
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should restart");
    let (wrong_runtime, _wrong_input_rx) = test_codex_runtime_handle(
        "orchestrator-stop-persist-failure-manual-recovery-wrong-runtime",
    );
    let baseline_message_count = {
        let mut inner = restarted.inner.lock().expect("state mutex poisoned");
        let reviewer_index = inner
            .find_session_index(&reviewer_session_id)
            .expect("reviewer session should exist after restart");
        inner.sessions[reviewer_index].runtime = SessionRuntime::Codex(wrong_runtime);
        inner.sessions[reviewer_index].session.messages.len()
    };

    let failed_recovery = restarted
        .dispatch_turn(
            &reviewer_session_id,
            SendMessageRequest {
                text: "this failed recovery should not clear the block".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
            },
        )
        .err()
        .expect("wrong runtime should reject the first manual recovery attempt");
    assert_eq!(failed_recovery.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        failed_recovery
            .message
            .contains("unexpected Codex runtime attached to Claude session")
    );

    {
        let inner = restarted.inner.lock().expect("state mutex poisoned");
        let reviewer = inner
            .sessions
            .iter()
            .find(|record| record.session.id == reviewer_session_id)
            .expect("reviewer session should still exist after failed manual recovery");
        assert_eq!(reviewer.session.status, SessionStatus::Idle);
        assert!(reviewer.orchestrator_auto_dispatch_blocked);
        assert_eq!(reviewer.queued_prompts.len(), 1);
        assert_eq!(reviewer.session.pending_prompts.len(), 1);
        assert_eq!(reviewer.session.messages.len(), baseline_message_count);
        assert!(reviewer.session.messages.iter().all(|message| !matches!(
            message,
            Message::Text { text, author: Author::You, .. }
                if text.contains("this failed recovery should not clear the block")
        )));
    }

    let (restart_reviewer_runtime, _restart_reviewer_input_rx) = test_claude_runtime_handle(
        "orchestrator-stop-persist-failure-manual-recovery-reviewer-restarted",
    );
    {
        let mut inner = restarted.inner.lock().expect("state mutex poisoned");
        let reviewer_index = inner
            .find_session_index(&reviewer_session_id)
            .expect("reviewer session should exist after restart");
        inner.sessions[reviewer_index].runtime = SessionRuntime::Claude(restart_reviewer_runtime);
    }

    let dispatch_result = restarted
        .dispatch_turn(
            &reviewer_session_id,
            SendMessageRequest {
                text: "please continue with a manual recovery prompt".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
            },
        )
        .expect("manual recovery prompt should dispatch");

    match dispatch_result {
        DispatchTurnResult::Dispatched(TurnDispatch::PersistentClaude { command, .. }) => {
            assert!(
                command
                    .text
                    .contains("please continue with a manual recovery prompt")
            );
        }
        DispatchTurnResult::Dispatched(_) => {
            panic!("manual recovery should dispatch on the reviewer Claude runtime")
        }
        DispatchTurnResult::Queued => panic!("manual recovery prompt should dispatch immediately"),
    }

    {
        let inner = restarted.inner.lock().expect("state mutex poisoned");
        let reviewer = inner
            .sessions
            .iter()
            .find(|record| record.session.id == reviewer_session_id)
            .expect("reviewer session should still exist after manual recovery");
        assert_eq!(reviewer.session.status, SessionStatus::Active);
        assert!(!reviewer.orchestrator_auto_dispatch_blocked);
        assert_eq!(reviewer.queued_prompts.len(), 1);
        assert_eq!(reviewer.session.pending_prompts.len(), 1);
        assert_eq!(
            reviewer.queued_prompts[0].pending_prompt.text,
            "reviewer queued work should stay behind the user prompt"
        );
        assert!(matches!(
            reviewer.session.messages.last(),
            Some(Message::Text { text, author: Author::You, .. })
                if text.contains("please continue with a manual recovery prompt")
        ));
    }

    drop(restarted);

    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(project_root);
}

// Tests that blocked session manual recovery preserves user prompt fifo after plain stop persist failure.
#[test]
fn blocked_session_manual_recovery_preserves_user_prompt_fifo_after_plain_stop_persist_failure() {
    let mut state = test_app_state();
    let original_persistence_path = state.persistence_path.clone();
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-stop-persist-failure-user-queue-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Blocked FIFO".to_owned()),
            workdir: Some(state.default_workdir.clone()),
            project_id: None,
            model: Some("claude-sonnet-4-5".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("session should be created");
    let session_id = created.session_id.clone();
    let (initial_runtime, _initial_input_rx) =
        test_claude_runtime_handle("plain-stop-persist-failure-initial-runtime");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(initial_runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].active_turn_start_message_count =
            Some(inner.sessions[index].session.messages.len());
        inner.sessions[index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::User,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-older-user-prompt".to_owned(),
                    timestamp: stamp_now(),
                    text: "older queued user prompt".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[index]);
        state.commit_locked(&mut inner).unwrap();
    }

    state.persistence_path = Arc::new(failing_persistence_path.clone());
    state.persist_tx = mpsc::channel().0;
    let stop_error = state
        .stop_session_with_options(&session_id, StopSessionOptions::default())
        .err()
        .expect("persist failures should abort plain stop persistence");
    assert_eq!(stop_error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        stop_error
            .message
            .contains("failed to persist session state")
    );

    state.persistence_path = original_persistence_path.clone();
    let (recovery_runtime, _recovery_input_rx) =
        test_claude_runtime_handle("plain-stop-persist-failure-recovery-runtime");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist after stop failure");
        inner.sessions[index].runtime = SessionRuntime::Claude(recovery_runtime);
        assert!(inner.sessions[index].orchestrator_auto_dispatch_blocked);
        assert_eq!(inner.sessions[index].queued_prompts.len(), 1);
        assert_eq!(
            inner.sessions[index].queued_prompts[0].source,
            QueuedPromptSource::User
        );
    }

    let dispatch_result = state
        .dispatch_turn(
            &session_id,
            SendMessageRequest {
                text: "new recovery prompt should stay behind old queued user work".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
            },
        )
        .expect("manual recovery should dispatch the oldest queued user prompt");

    match dispatch_result {
        DispatchTurnResult::Dispatched(TurnDispatch::PersistentClaude { command, .. }) => {
            assert_eq!(command.text, "older queued user prompt");
        }
        DispatchTurnResult::Dispatched(_) => {
            panic!("plain blocked FIFO recovery should dispatch on the Claude runtime")
        }
        DispatchTurnResult::Queued => {
            panic!("plain blocked FIFO recovery should dispatch immediately")
        }
    }

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == session_id)
            .expect("session should still exist after recovery dispatch");
        assert_eq!(record.session.status, SessionStatus::Active);
        assert!(!record.orchestrator_auto_dispatch_blocked);
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(record.session.pending_prompts.len(), 1);
        assert_eq!(
            record.queued_prompts[0].pending_prompt.text,
            "new recovery prompt should stay behind old queued user work"
        );
        assert_eq!(record.queued_prompts[0].source, QueuedPromptSource::User);
    }

    let _ = fs::remove_file(original_persistence_path.as_path());
    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Tests that blocked session manual recovery prioritizes existing user queue ahead of stale orchestrator work.
#[test]
fn blocked_session_manual_recovery_prioritizes_existing_user_queue_ahead_of_stale_orchestrator_work()
 {
    let mut state = test_app_state();
    let original_persistence_path = state.persistence_path.clone();
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-stop-persist-failure-mixed-queue-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Blocked Mixed Queue".to_owned()),
            workdir: Some(state.default_workdir.clone()),
            project_id: None,
            model: Some("claude-sonnet-4-5".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("session should be created");
    let session_id = created.session_id.clone();
    let (initial_runtime, _initial_input_rx) =
        test_claude_runtime_handle("mixed-queue-persist-failure-initial-runtime");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(initial_runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].active_turn_start_message_count =
            Some(inner.sessions[index].session.messages.len());
        inner.sessions[index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-stale-orchestrator-prompt".to_owned(),
                    timestamp: stamp_now(),
                    text: "older stale orchestrator prompt".to_owned(),
                    expanded_text: None,
                },
            });
        inner.sessions[index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::User,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-older-user-prompt-mixed".to_owned(),
                    timestamp: stamp_now(),
                    text: "older queued user prompt behind stale orchestrator work".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[index]);
        state.commit_locked(&mut inner).unwrap();
    }

    state.persistence_path = Arc::new(failing_persistence_path.clone());
    state.persist_tx = mpsc::channel().0;
    let stop_error = state
        .stop_session_with_options(&session_id, StopSessionOptions::default())
        .err()
        .expect("persist failures should abort plain stop persistence");
    assert_eq!(stop_error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        stop_error
            .message
            .contains("failed to persist session state")
    );

    state.persistence_path = original_persistence_path.clone();
    let (recovery_runtime, _recovery_input_rx) =
        test_claude_runtime_handle("mixed-queue-persist-failure-recovery-runtime");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist after stop failure");
        inner.sessions[index].runtime = SessionRuntime::Claude(recovery_runtime);
        assert!(inner.sessions[index].orchestrator_auto_dispatch_blocked);
        assert_eq!(inner.sessions[index].queued_prompts.len(), 2);
        assert_eq!(
            inner.sessions[index].queued_prompts[0].source,
            QueuedPromptSource::Orchestrator
        );
        assert_eq!(
            inner.sessions[index].queued_prompts[1].source,
            QueuedPromptSource::User
        );
    }

    let dispatch_result = state
        .dispatch_turn(
            &session_id,
            SendMessageRequest {
                text: "new manual recovery prompt should not jump ahead of older queued user work"
                    .to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
            },
        )
        .expect("manual recovery should dispatch the older queued user prompt first");

    match dispatch_result {
        DispatchTurnResult::Dispatched(TurnDispatch::PersistentClaude { command, .. }) => {
            assert_eq!(
                command.text,
                "older queued user prompt behind stale orchestrator work"
            );
        }
        DispatchTurnResult::Dispatched(_) => {
            panic!("mixed blocked recovery should dispatch on the Claude runtime")
        }
        DispatchTurnResult::Queued => {
            panic!("mixed blocked recovery should dispatch immediately")
        }
    }

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == session_id)
            .expect("session should still exist after mixed recovery dispatch");
        assert_eq!(record.session.status, SessionStatus::Active);
        assert!(!record.orchestrator_auto_dispatch_blocked);
        assert_eq!(record.queued_prompts.len(), 2);
        assert_eq!(
            record.queued_prompts[0].pending_prompt.text,
            "new manual recovery prompt should not jump ahead of older queued user work"
        );
        assert_eq!(record.queued_prompts[0].source, QueuedPromptSource::User);
        assert_eq!(
            record.queued_prompts[1].pending_prompt.text,
            "older stale orchestrator prompt"
        );
        assert_eq!(
            record.queued_prompts[1].source,
            QueuedPromptSource::Orchestrator
        );
    }

    let _ = fs::remove_file(original_persistence_path.as_path());
    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Tests that aborted stop does not relaunch child work completed during stop.
#[test]
fn aborted_stop_does_not_relaunch_child_work_completed_during_stop() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-orchestrator-stop-guard-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("stop guard project root should exist");
    let project_id = create_test_project(&state, &project_root, "Stop Guard Project");
    let mut draft = sample_orchestrator_template_draft();
    draft.transitions.push(OrchestratorTemplateTransition {
        id: "planner-to-reviewer-during-stop".to_owned(),
        from_session_id: "planner".to_owned(),
        to_session_id: "reviewer".to_owned(),
        from_anchor: Some("right".to_owned()),
        to_anchor: Some("top".to_owned()),
        trigger: OrchestratorTransitionTrigger::OnCompletion,
        result_mode: OrchestratorTransitionResultMode::LastResponse,
        prompt_template: Some("Review this plan directly:\n\n{{result}}".to_owned()),
    });
    let template = state
        .create_orchestrator_template(draft)
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let reviewer_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "reviewer")
        .expect("reviewer session should be mapped")
        .session_id
        .clone();
    let (planner_runtime, _planner_input_rx) =
        test_claude_runtime_handle("orchestrator-stop-guard-planner");
    let (builder_runtime, _builder_input_rx) =
        test_codex_runtime_handle("orchestrator-stop-guard-builder");

    let planner_token = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        let reviewer_index = inner
            .find_session_index(&reviewer_session_id)
            .expect("reviewer session should exist");

        inner.sessions[planner_index].runtime = SessionRuntime::Claude(planner_runtime);
        inner.sessions[planner_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index].runtime = SessionRuntime::Codex(builder_runtime);
        inner.sessions[builder_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-builder-orchestrator-stop-follow-up".to_owned(),
                    timestamp: stamp_now(),
                    text: "builder follow-up that should be cleared on aborted stop".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[builder_index]);
        inner.sessions[reviewer_index].session.status = SessionStatus::Active;

        let message_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_index],
            Message::Text {
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Implement the panel dragging changes.".to_owned(),
                expanded_text: None,
            },
        );
        inner.sessions[planner_index].session.preview =
            "Implement the panel dragging changes.".to_owned();
        inner.sessions[planner_index]
            .runtime
            .runtime_token()
            .expect("planner runtime token should exist")
    };

    state
        .begin_orchestrator_stop(&instance_id)
        .expect("stop guard should be acquired");
    state
        .finish_turn_ok_if_runtime_matches(&planner_session_id, &planner_token)
        .expect("planner completion should succeed while stop is in flight");

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        assert_eq!(instance.pending_transitions.len(), 2);
        assert!(
            instance
                .pending_transitions
                .iter()
                .any(|pending| { pending.destination_session_id == builder_session_id })
        );
        assert!(
            instance
                .pending_transitions
                .iter()
                .any(|pending| { pending.destination_session_id == reviewer_session_id })
        );
    }

    state
        .stop_session_with_options(
            &builder_session_id,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: false,
                orchestrator_stop_instance_id: None,
            },
        )
        .expect("builder stop should succeed while the orchestrator stop is in flight");
    state.note_stopped_orchestrator_session(&instance_id, &builder_session_id);
    state
        .prune_pending_transitions_for_stopped_orchestrator_sessions(&instance_id)
        .expect("aborted stops should prune pending work for stopped children");

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        let planner_instance = instance
            .session_instances
            .iter()
            .find(|candidate| candidate.session_id == planner_session_id)
            .expect("planner instance should exist");
        assert_eq!(instance.pending_transitions.len(), 1);
        assert!(
            instance
                .pending_transitions
                .iter()
                .all(|pending| { pending.destination_session_id == reviewer_session_id })
        );
        assert_ne!(
            planner_instance.last_completion_revision,
            planner_instance.last_delivered_completion_revision
        );
        let builder = inner
            .sessions
            .iter()
            .find(|record| record.session.id == builder_session_id)
            .expect("builder session should exist");
        assert_eq!(builder.session.status, SessionStatus::Idle);
        assert!(matches!(builder.runtime, SessionRuntime::None));
        assert!(builder.queued_prompts.is_empty());
        assert!(builder.session.pending_prompts.is_empty());
    }

    let reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    let reloaded_builder = reloaded_inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("persisted builder session should exist");
    assert!(reloaded_builder.queued_prompts.is_empty());
    assert!(reloaded_builder.session.pending_prompts.is_empty());

    state.finish_orchestrator_stop(&instance_id);
    state
        .resume_pending_orchestrator_transitions()
        .expect("aborted stops should resume completions for unstopped children");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let instance = inner
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("orchestrator should exist");
    let planner_instance = instance
        .session_instances
        .iter()
        .find(|candidate| candidate.session_id == planner_session_id)
        .expect("planner instance should exist");
    assert!(instance.pending_transitions.is_empty());
    assert_eq!(
        planner_instance.last_completion_revision,
        planner_instance.last_delivered_completion_revision
    );
    let builder = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("builder session should exist");
    assert_eq!(builder.session.status, SessionStatus::Idle);
    assert!(matches!(builder.runtime, SessionRuntime::None));
    assert!(builder.queued_prompts.is_empty());
    assert!(builder.session.pending_prompts.is_empty());
    let reviewer = inner
        .sessions
        .iter()
        .find(|record| record.session.id == reviewer_session_id)
        .expect("reviewer session should exist");
    assert_eq!(reviewer.session.status, SessionStatus::Active);
    assert_eq!(reviewer.queued_prompts.len(), 1);
    assert_eq!(reviewer.session.pending_prompts.len(), 1);
    assert_eq!(
        reviewer.queued_prompts[0].source,
        QueuedPromptSource::Orchestrator
    );
    assert!(
        reviewer.session.pending_prompts[0]
            .text
            .contains("Implement the panel dragging changes.")
    );

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that begin orchestrator stop cleans up guards on missing and stopped errors.
#[test]
fn begin_orchestrator_stop_cleans_up_guards_on_missing_and_stopped_errors() {
    let state = test_app_state();
    let missing_instance_id = "missing-orchestrator-instance";
    let error = state
        .begin_orchestrator_stop(missing_instance_id)
        .expect_err("missing orchestrators should not start a stop");
    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert_eq!(error.message, "orchestrator instance not found");
    assert!(
        !state
            .stopping_orchestrator_ids
            .lock()
            .expect("orchestrator stop mutex poisoned")
            .contains(missing_instance_id)
    );
    assert!(
        !state
            .stopping_orchestrator_session_ids
            .lock()
            .expect("orchestrator stop session mutex poisoned")
            .contains_key(missing_instance_id)
    );

    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-begin-errors-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let project_id = create_test_project(&state, &project_root, "Begin Stop Errors Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter_mut()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        instance.status = OrchestratorInstanceStatus::Stopped;
        instance.stop_in_progress = false;
    }

    let error = state
        .begin_orchestrator_stop(&instance_id)
        .expect_err("stopped orchestrators should reject stop");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, "orchestrator is already stopped");
    assert!(
        !state
            .stopping_orchestrator_ids
            .lock()
            .expect("orchestrator stop mutex poisoned")
            .contains(&instance_id)
    );
    assert!(
        !state
            .stopping_orchestrator_session_ids
            .lock()
            .expect("orchestrator stop session mutex poisoned")
            .contains_key(&instance_id)
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let instance = inner
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("orchestrator should still exist");
    assert_eq!(instance.status, OrchestratorInstanceStatus::Stopped);
    assert!(!instance.stop_in_progress);
    assert!(instance.active_session_ids_during_stop.is_none());
    drop(inner);

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that begin orchestrator stop rolls back stop in progress after persist failure.
#[test]
fn begin_orchestrator_stop_rolls_back_stop_in_progress_after_persist_failure() {
    let mut state = test_app_state();
    let original_persistence_path = state.persistence_path.clone();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-begin-persist-failure-{}",
        Uuid::new_v4()
    ));
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-begin-persist-failure-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");
    let project_id = create_test_project(&state, &project_root, "Begin Stop Persist Failure");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    state.persist_tx = mpsc::channel().0;

    let error = state
        .begin_orchestrator_stop(&instance_id)
        .expect_err("persistence failures should abort stop initialization");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        error
            .message
            .contains("failed to persist orchestrator stop state")
    );
    assert!(
        !state
            .stopping_orchestrator_ids
            .lock()
            .expect("orchestrator stop mutex poisoned")
            .contains(&instance_id)
    );
    assert!(
        !state
            .stopping_orchestrator_session_ids
            .lock()
            .expect("orchestrator stop session mutex poisoned")
            .contains_key(&instance_id)
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let instance = inner
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("orchestrator should still exist");
    assert_eq!(instance.status, OrchestratorInstanceStatus::Running);
    assert!(!instance.stop_in_progress);
    assert!(instance.active_session_ids_during_stop.is_none());
    drop(inner);

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(original_persistence_path.as_path());
    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Tests that load state preserves pending transitions when stop in progress has no stopped children.
#[test]
fn load_state_preserves_pending_transitions_when_stop_in_progress_has_no_stopped_children() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-restart-{}",
        Uuid::new_v4()
    ));
    let state_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-restart-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("restart recovery project root should exist");
    fs::create_dir_all(&state_root).expect("restart recovery state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");

    let mut state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    // Force synchronous persistence so file reads in this test see the
    // written data immediately.
    state.persist_tx = mpsc::channel().0;
    let project_id = create_test_project(&state, &project_root, "Restart Recovery Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let reviewer_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "reviewer")
        .expect("reviewer session should be mapped")
        .session_id
        .clone();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let instance_index = inner
            .orchestrator_instances
            .iter()
            .position(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        let completion_revision = inner.revision.saturating_add(1);
        inner.orchestrator_instances[instance_index].stop_in_progress = true;
        inner.orchestrator_instances[instance_index].active_session_ids_during_stop =
            Some(vec![builder_session_id.clone()]);
        inner.orchestrator_instances[instance_index]
            .pending_transitions
            .push(PendingTransition {
                id: format!("pending-transition-{}", Uuid::new_v4()),
                transition_id: "planner-to-reviewer".to_owned(),
                source_session_id: planner_session_id.clone(),
                destination_session_id: reviewer_session_id.clone(),
                completion_revision,
                rendered_prompt: "reviewer work should survive recovery".to_owned(),
                created_at: stamp_orchestrator_template_now(),
            });
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        inner.sessions[builder_index].session.status = SessionStatus::Active;
        state
            .persist_internal_locked(&inner)
            .expect("stop recovery state should persist");
    }

    drop(state);

    let recovered = load_state(&persistence_path)
        .expect("recovered state should load")
        .expect("recovered state should exist");
    let recovered_instance = recovered
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("recovered orchestrator should exist");
    assert_eq!(
        recovered_instance.status,
        OrchestratorInstanceStatus::Running
    );
    assert!(!recovered_instance.stop_in_progress);
    assert!(recovered_instance.active_session_ids_during_stop.is_none());
    assert!(
        recovered_instance
            .stopped_session_ids_during_stop
            .is_empty()
    );
    assert_eq!(recovered_instance.pending_transitions.len(), 1);
    assert_eq!(
        recovered_instance.pending_transitions[0].destination_session_id,
        reviewer_session_id
    );
    let recovered_builder = recovered
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("persisted builder session should exist after restart");
    assert_eq!(recovered_builder.session.status, SessionStatus::Error);
    let _ = fs::remove_file(persistence_path);
    let _ = fs::remove_file(orchestrator_templates_path);
    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(project_root);
}

// Tests that load state recovers completed stop when active children finished during stop.
#[test]
fn load_state_recovers_completed_stop_when_active_children_finished_during_stop() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-restart-finished-{}",
        Uuid::new_v4()
    ));
    let state_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-restart-finished-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("restart recovery project root should exist");
    fs::create_dir_all(&state_root).expect("restart recovery state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");

    let mut state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    // Force synchronous persistence so file reads in this test see the
    // written data immediately.
    state.persist_tx = mpsc::channel().0;
    let project_id = create_test_project(&state, &project_root, "Restart Recovery Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (planner_runtime, _planner_input_rx) =
        test_claude_runtime_handle("orchestrator-stop-restart-planner");

    let planner_token = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        inner.sessions[planner_index].runtime = SessionRuntime::Claude(planner_runtime);
        inner.sessions[planner_index].session.status = SessionStatus::Active;

        let message_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_index],
            Message::Text {
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Implement the panel dragging changes.".to_owned(),
                expanded_text: None,
            },
        );
        inner.sessions[planner_index].session.preview =
            "Implement the panel dragging changes.".to_owned();
        inner.sessions[planner_index]
            .runtime
            .runtime_token()
            .expect("planner runtime token should exist")
    };

    state
        .begin_orchestrator_stop(&instance_id)
        .expect("stop should be marked in progress");
    state
        .finish_turn_ok_if_runtime_matches(&planner_session_id, &planner_token)
        .expect("planner completion should persist while stop is in flight");

    let persisted_mid_stop: Value = serde_json::from_slice(
        &fs::read(&persistence_path).expect("mid-stop state file should exist"),
    )
    .expect("mid-stop state should deserialize");
    let persisted_mid_stop_instance = persisted_mid_stop["orchestratorInstances"]
        .as_array()
        .expect("persisted orchestrator instances should be present")
        .iter()
        .find(|candidate| candidate["id"] == instance_id)
        .expect("persisted orchestrator should exist");
    assert_eq!(
        persisted_mid_stop_instance["status"],
        Value::String("running".to_owned())
    );
    assert_eq!(
        persisted_mid_stop_instance["stopInProgress"],
        Value::Bool(true)
    );
    assert_eq!(
        persisted_mid_stop_instance["pendingTransitions"]
            .as_array()
            .expect("pending transitions should be present")
            .len(),
        1
    );
    assert_eq!(
        persisted_mid_stop_instance["activeSessionIdsDuringStop"]
            .as_array()
            .expect("active stop session ids should be present")
            .len(),
        1
    );
    assert_eq!(
        persisted_mid_stop_instance["activeSessionIdsDuringStop"][0],
        Value::String(planner_session_id.clone())
    );

    drop(state);

    let recovered = load_state(&persistence_path)
        .expect("recovered state should load")
        .expect("recovered state should exist");
    let recovered_instance = recovered
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("recovered orchestrator should exist");
    assert_eq!(
        recovered_instance.status,
        OrchestratorInstanceStatus::Stopped
    );
    assert!(!recovered_instance.stop_in_progress);
    assert!(recovered_instance.active_session_ids_during_stop.is_none());
    assert!(
        recovered_instance
            .stopped_session_ids_during_stop
            .is_empty()
    );
    assert!(recovered_instance.pending_transitions.is_empty());
    assert!(recovered_instance.completed_at.is_some());
    let recovered_builder = recovered
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("persisted builder session should exist after restart");
    assert!(recovered_builder.queued_prompts.is_empty());
    assert!(recovered_builder.session.pending_prompts.is_empty());
    let _ = fs::remove_file(persistence_path);
    let _ = fs::remove_file(orchestrator_templates_path);
    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(project_root);
}

// Tests that load state prunes only stopped child work when recovering stop in progress.
#[test]
fn load_state_prunes_only_stopped_child_work_when_recovering_stop_in_progress() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-recovery-queued-{}",
        Uuid::new_v4()
    ));
    let state_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-recovery-queued-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("recovery project root should exist");
    fs::create_dir_all(&state_root).expect("recovery state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");

    let mut state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    state.persist_tx = mpsc::channel().0;
    let project_id = create_test_project(&state, &project_root, "Recovery Queue Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let reviewer_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "reviewer")
        .expect("reviewer session should be mapped")
        .session_id
        .clone();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let instance_index = inner
            .orchestrator_instances
            .iter()
            .position(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        let completion_revision = inner.revision.saturating_add(1);
        inner.orchestrator_instances[instance_index].stop_in_progress = true;
        inner.orchestrator_instances[instance_index].active_session_ids_during_stop = Some(vec![
            builder_session_id.clone(),
            reviewer_session_id.clone(),
        ]);
        inner.orchestrator_instances[instance_index]
            .stopped_session_ids_during_stop
            .push(builder_session_id.clone());
        inner.orchestrator_instances[instance_index]
            .pending_transitions
            .push(PendingTransition {
                id: format!("pending-transition-{}", Uuid::new_v4()),
                transition_id: "planner-to-builder".to_owned(),
                source_session_id: planner_session_id.clone(),
                destination_session_id: builder_session_id.clone(),
                completion_revision,
                rendered_prompt: "stale stop recovery prompt".to_owned(),
                created_at: stamp_orchestrator_template_now(),
            });
        inner.orchestrator_instances[instance_index]
            .pending_transitions
            .push(PendingTransition {
                id: format!("pending-transition-{}", Uuid::new_v4()),
                transition_id: "planner-to-reviewer".to_owned(),
                source_session_id: planner_session_id.clone(),
                destination_session_id: reviewer_session_id.clone(),
                completion_revision,
                rendered_prompt: "reviewer work should survive recovery".to_owned(),
                created_at: stamp_orchestrator_template_now(),
            });
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        let reviewer_index = inner
            .find_session_index(&reviewer_session_id)
            .expect("reviewer session should exist");
        inner.sessions[reviewer_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-recovery-orchestrator-prompt".to_owned(),
                    timestamp: stamp_now(),
                    text: "stale queued orchestrator prompt".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[builder_index]);
        state
            .persist_internal_locked(&inner)
            .expect("stop recovery state should persist");
    }

    drop(state);

    let recovered = load_state(&persistence_path)
        .expect("recovered state should load")
        .expect("recovered state should exist");
    let recovered_instance = recovered
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("recovered orchestrator should exist");
    assert_eq!(
        recovered_instance.status,
        OrchestratorInstanceStatus::Running
    );
    assert!(!recovered_instance.stop_in_progress);
    assert!(recovered_instance.active_session_ids_during_stop.is_none());
    assert!(
        recovered_instance
            .stopped_session_ids_during_stop
            .is_empty()
    );
    assert_eq!(recovered_instance.pending_transitions.len(), 1);
    assert_eq!(
        recovered_instance.pending_transitions[0].destination_session_id,
        reviewer_session_id
    );
    let recovered_builder = recovered
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("persisted builder session should exist after restart");
    assert!(recovered_builder.queued_prompts.is_empty());
    assert!(recovered_builder.session.pending_prompts.is_empty());
    let _ = fs::remove_file(persistence_path);
    let _ = fs::remove_file(orchestrator_templates_path);
    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(project_root);
}

// Tests that load state recovers completed stop when all active children were stopped.
#[test]
fn load_state_recovers_completed_stop_when_all_active_children_were_stopped() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-recovery-complete-{}",
        Uuid::new_v4()
    ));
    let state_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-recovery-complete-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("completed recovery project root should exist");
    fs::create_dir_all(&state_root).expect("completed recovery state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");

    let mut state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    state.persist_tx = mpsc::channel().0;
    let project_id = create_test_project(&state, &project_root, "Completed Recovery Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let reviewer_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "reviewer")
        .expect("reviewer session should be mapped")
        .session_id
        .clone();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let instance_index = inner
            .orchestrator_instances
            .iter()
            .position(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        let completion_revision = inner.revision.saturating_add(1);
        inner.orchestrator_instances[instance_index].stop_in_progress = true;
        inner.orchestrator_instances[instance_index].active_session_ids_during_stop =
            Some(vec![builder_session_id.clone()]);
        inner.orchestrator_instances[instance_index]
            .stopped_session_ids_during_stop
            .push(builder_session_id.clone());
        inner.orchestrator_instances[instance_index]
            .pending_transitions
            .push(PendingTransition {
                id: format!("pending-transition-{}", Uuid::new_v4()),
                transition_id: "builder-to-reviewer".to_owned(),
                source_session_id: builder_session_id.clone(),
                destination_session_id: reviewer_session_id.clone(),
                completion_revision,
                rendered_prompt: "idle reviewer work should be discarded".to_owned(),
                created_at: stamp_orchestrator_template_now(),
            });
        let reviewer_index = inner
            .find_session_index(&reviewer_session_id)
            .expect("reviewer session should exist");
        inner.sessions[reviewer_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-completed-stop-reviewer-prompt".to_owned(),
                    timestamp: stamp_now(),
                    text: "queued reviewer work should be discarded".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[reviewer_index]);
        state
            .persist_internal_locked(&inner)
            .expect("completed stop recovery state should persist");
    }

    drop(state);

    let recovered = load_state(&persistence_path)
        .expect("recovered state should load")
        .expect("recovered state should exist");
    let recovered_instance = recovered
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("recovered orchestrator should exist");
    assert_eq!(
        recovered_instance.status,
        OrchestratorInstanceStatus::Stopped
    );
    assert!(!recovered_instance.stop_in_progress);
    assert!(recovered_instance.active_session_ids_during_stop.is_none());
    assert!(
        recovered_instance
            .stopped_session_ids_during_stop
            .is_empty()
    );
    assert!(recovered_instance.pending_transitions.is_empty());
    assert!(recovered_instance.completed_at.is_some());
    let recovered_reviewer = recovered
        .sessions
        .iter()
        .find(|record| record.session.id == reviewer_session_id)
        .expect("persisted reviewer session should exist after restart");
    assert!(recovered_reviewer.queued_prompts.is_empty());
    assert!(recovered_reviewer.session.pending_prompts.is_empty());
    let _ = fs::remove_file(persistence_path);
    let _ = fs::remove_file(orchestrator_templates_path);
    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(project_root);
}

// Tests that orchestrator templates round-trip through draft conversion helpers.
#[test]
fn orchestrator_template_draft_round_trips_through_template_helpers() {
    let draft = sample_orchestrator_template_draft();
    let template = orchestrator_template_from_draft("template-round-trip", draft.clone())
        .expect("sample draft should normalize into a template");
    let round_tripped = orchestrator_template_to_draft(&template);

    assert_eq!(round_tripped, draft);
}

fn sample_orchestrator_template_draft() -> OrchestratorTemplateDraft {
    OrchestratorTemplateDraft {
        name: "Feature Delivery Flow".to_owned(),
        description: "Coordinate implementation and review.".to_owned(),
        project_id: None,
        sessions: vec![
            OrchestratorSessionTemplate {
                id: "planner".to_owned(),
                name: "Planner".to_owned(),
                agent: Agent::Claude,
                model: Some("claude-sonnet-4-5".to_owned()),
                instructions: "Plan the work and decide the next action.".to_owned(),
                auto_approve: false,
                input_mode: OrchestratorSessionInputMode::Queue,
                position: OrchestratorNodePosition { x: 620.0, y: 120.0 },
            },
            OrchestratorSessionTemplate {
                id: "builder".to_owned(),
                name: "Builder".to_owned(),
                agent: Agent::Codex,
                model: Some("gpt-5".to_owned()),
                instructions: "Implement the requested changes.".to_owned(),
                auto_approve: true,
                input_mode: OrchestratorSessionInputMode::Queue,
                position: OrchestratorNodePosition { x: 180.0, y: 420.0 },
            },
            OrchestratorSessionTemplate {
                id: "reviewer".to_owned(),
                name: "Reviewer".to_owned(),
                agent: Agent::Claude,
                model: Some("claude-sonnet-4-5".to_owned()),
                instructions: "Review the produced changes and summarize issues.".to_owned(),
                auto_approve: false,
                input_mode: OrchestratorSessionInputMode::Queue,
                position: OrchestratorNodePosition { x: 980.0, y: 420.0 },
            },
        ],
        transitions: vec![
            OrchestratorTemplateTransition {
                id: "planner-to-builder".to_owned(),
                from_session_id: "planner".to_owned(),
                to_session_id: "builder".to_owned(),
                from_anchor: Some("bottom".to_owned()),
                to_anchor: Some("top".to_owned()),
                trigger: OrchestratorTransitionTrigger::OnCompletion,
                result_mode: OrchestratorTransitionResultMode::LastResponse,
                prompt_template: Some(
                    "Use this plan and implement it:

{{result}}"
                        .to_owned(),
                ),
            },
            OrchestratorTemplateTransition {
                id: "builder-to-reviewer".to_owned(),
                from_session_id: "builder".to_owned(),
                to_session_id: "reviewer".to_owned(),
                from_anchor: Some("right".to_owned()),
                to_anchor: Some("left".to_owned()),
                trigger: OrchestratorTransitionTrigger::OnCompletion,
                result_mode: OrchestratorTransitionResultMode::SummaryAndLastResponse,
                prompt_template: Some(
                    "Review this implementation:

{{result}}"
                        .to_owned(),
                ),
            },
        ],
    }
}

fn sample_deadlocked_orchestrator_template_draft() -> OrchestratorTemplateDraft {
    OrchestratorTemplateDraft {
        name: "Consolidate Deadlock Flow".to_owned(),
        description: "Exercise remote deadlock skipping.".to_owned(),
        project_id: None,
        sessions: vec![
            OrchestratorSessionTemplate {
                id: "source-a".to_owned(),
                name: "Source A".to_owned(),
                agent: Agent::Claude,
                model: Some("claude-sonnet-4-5".to_owned()),
                instructions: "Provide the first source input.".to_owned(),
                auto_approve: false,
                input_mode: OrchestratorSessionInputMode::Queue,
                position: OrchestratorNodePosition { x: 120.0, y: 160.0 },
            },
            OrchestratorSessionTemplate {
                id: "source-b".to_owned(),
                name: "Source B".to_owned(),
                agent: Agent::Claude,
                model: Some("claude-sonnet-4-5".to_owned()),
                instructions: "Provide the second source input.".to_owned(),
                auto_approve: false,
                input_mode: OrchestratorSessionInputMode::Queue,
                position: OrchestratorNodePosition { x: 120.0, y: 460.0 },
            },
            OrchestratorSessionTemplate {
                id: "consolidate-a".to_owned(),
                name: "Consolidate A".to_owned(),
                agent: Agent::Claude,
                model: Some("claude-sonnet-4-5".to_owned()),
                instructions: "Wait on source A and consolidate B.".to_owned(),
                auto_approve: false,
                input_mode: OrchestratorSessionInputMode::Consolidate,
                position: OrchestratorNodePosition { x: 760.0, y: 160.0 },
            },
            OrchestratorSessionTemplate {
                id: "consolidate-b".to_owned(),
                name: "Consolidate B".to_owned(),
                agent: Agent::Claude,
                model: Some("claude-sonnet-4-5".to_owned()),
                instructions: "Wait on source B and consolidate A.".to_owned(),
                auto_approve: false,
                input_mode: OrchestratorSessionInputMode::Consolidate,
                position: OrchestratorNodePosition { x: 760.0, y: 460.0 },
            },
        ],
        transitions: vec![
            OrchestratorTemplateTransition {
                id: "source-a-to-consolidate-a".to_owned(),
                from_session_id: "source-a".to_owned(),
                to_session_id: "consolidate-a".to_owned(),
                from_anchor: Some("right".to_owned()),
                to_anchor: Some("left".to_owned()),
                trigger: OrchestratorTransitionTrigger::OnCompletion,
                result_mode: OrchestratorTransitionResultMode::Summary,
                prompt_template: Some(
                    "Source A summary:

{{result}}"
                        .to_owned(),
                ),
            },
            OrchestratorTemplateTransition {
                id: "consolidate-b-to-consolidate-a".to_owned(),
                from_session_id: "consolidate-b".to_owned(),
                to_session_id: "consolidate-a".to_owned(),
                from_anchor: Some("top".to_owned()),
                to_anchor: Some("bottom".to_owned()),
                trigger: OrchestratorTransitionTrigger::OnCompletion,
                result_mode: OrchestratorTransitionResultMode::Summary,
                prompt_template: Some(
                    "Consolidate B summary:

{{result}}"
                        .to_owned(),
                ),
            },
            OrchestratorTemplateTransition {
                id: "source-b-to-consolidate-b".to_owned(),
                from_session_id: "source-b".to_owned(),
                to_session_id: "consolidate-b".to_owned(),
                from_anchor: Some("right".to_owned()),
                to_anchor: Some("left".to_owned()),
                trigger: OrchestratorTransitionTrigger::OnCompletion,
                result_mode: OrchestratorTransitionResultMode::Summary,
                prompt_template: Some(
                    "Source B summary:

{{result}}"
                        .to_owned(),
                ),
            },
            OrchestratorTemplateTransition {
                id: "consolidate-a-to-consolidate-b".to_owned(),
                from_session_id: "consolidate-a".to_owned(),
                to_session_id: "consolidate-b".to_owned(),
                from_anchor: Some("bottom".to_owned()),
                to_anchor: Some("top".to_owned()),
                trigger: OrchestratorTransitionTrigger::OnCompletion,
                result_mode: OrchestratorTransitionResultMode::Summary,
                prompt_template: Some(
                    "Consolidate A summary:

{{result}}"
                        .to_owned(),
                ),
            },
        ],
    }
}

// Tests that start_turn_on_record rejects remote proxy sessions directly.
#[test]
fn start_turn_on_record_rejects_remote_proxy_sessions() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    inner.sessions[index].remote_id = Some("ssh-lab".to_owned());
    inner.sessions[index].remote_session_id = Some("remote-session-1".to_owned());

    let error = match state.start_turn_on_record(
        &mut inner.sessions[index],
        "message-remote-proxy".to_owned(),
        "Dispatch through the remote backend.".to_owned(),
        Vec::new(),
        None,
    ) {
        Ok(_) => panic!("remote proxy sessions should reject local turn dispatch"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        error.message,
        "remote proxy sessions must dispatch through the remote backend"
    );
    assert!(
        inner.sessions[index]
            .active_turn_start_message_count
            .is_none()
    );
    assert!(inner.sessions[index].session.messages.is_empty());
    assert!(inner.sessions[index].session.pending_prompts.is_empty());

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that failed orchestrator transition dispatch becomes a visible destination error.
#[test]
fn failed_orchestrator_transition_dispatch_becomes_a_visible_destination_error() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-transition-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("transition failure project root should exist");
    let project_id = state
        .create_project(CreateProjectRequest {
            name: Some("Transition Failure Project".to_owned()),
            root_path: project_root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .expect("project should be created")
        .project_id;
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (runtime, input_rx) = test_codex_runtime_handle("orchestrator-transition-failure");
    drop(input_rx);

    let completion_revision = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");

        inner.sessions[builder_index].runtime = SessionRuntime::Codex(runtime);
        let message_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_index],
            Message::Text {
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Implement the panel dragging changes.".to_owned(),
                expanded_text: None,
            },
        );
        inner.sessions[planner_index].session.status = SessionStatus::Idle;

        let completion_revision = inner.revision + 1;
        schedule_orchestrator_transitions_for_completed_session(
            &mut inner,
            &HashMap::new(),
            &planner_session_id,
            completion_revision,
        );
        state.commit_locked(&mut inner).unwrap();
        completion_revision
    };

    state
        .resume_pending_orchestrator_transitions()
        .expect("transition handoff should stay durable even if runtime delivery fails");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let planner_instance = inner.orchestrator_instances[0]
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner instance should exist");
    let builder = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("builder session should exist");
    assert_eq!(
        planner_instance.last_delivered_completion_revision,
        Some(completion_revision)
    );
    assert_eq!(builder.session.status, SessionStatus::Error);
    assert!(builder.queued_prompts.is_empty());
    assert!(builder.session.pending_prompts.is_empty());
    assert!(matches!(
        builder.session.messages.first(),
        Some(Message::Text {
            author: Author::You,
            text,
            ..
        }) if text.contains("Implement the panel dragging changes.")
    ));
    assert!(matches!(
        builder.session.messages.last(),
        Some(Message::Text {
            author: Author::Assistant,
            text,
            ..
        }) if text.contains("failed to queue prompt for Codex session")
    ));
    assert!(
        inner.orchestrator_instances[0]
            .pending_transitions
            .is_empty()
    );
}

// Tests that failed orchestrator transition dispatch does not block other instances.
#[test]
fn failed_orchestrator_transition_dispatch_does_not_block_other_instances() {
    let state = test_app_state();
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;

    let project_root_a =
        std::env::temp_dir().join(format!("termal-orchestrator-multi-a-{}", Uuid::new_v4()));
    let project_root_b =
        std::env::temp_dir().join(format!("termal-orchestrator-multi-b-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root_a).expect("first project root should exist");
    fs::create_dir_all(&project_root_b).expect("second project root should exist");

    let project_id_a = create_test_project(&state, &project_root_a, "Multi A");
    let project_id_b = create_test_project(&state, &project_root_b, "Multi B");

    let orchestrator_a = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id.clone(),
            project_id: Some(project_id_a),
            template: None,
        })
        .expect("first orchestrator instance should be created")
        .orchestrator;
    let orchestrator_b = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id_b),
            template: None,
        })
        .expect("second orchestrator instance should be created")
        .orchestrator;

    let planner_a_session_id = orchestrator_a
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("first planner session should be mapped")
        .session_id
        .clone();
    let builder_a_session_id = orchestrator_a
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("first builder session should be mapped")
        .session_id
        .clone();
    let planner_b_session_id = orchestrator_b
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("second planner session should be mapped")
        .session_id
        .clone();
    let builder_b_session_id = orchestrator_b
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("second builder session should be mapped")
        .session_id
        .clone();
    let (failing_runtime, failing_input_rx) =
        test_codex_runtime_handle("orchestrator-transition-failure-a");
    drop(failing_input_rx);

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_a_index = inner
            .find_session_index(&planner_a_session_id)
            .expect("first planner session should exist");
        let builder_a_index = inner
            .find_session_index(&builder_a_session_id)
            .expect("first builder session should exist");
        let planner_b_index = inner
            .find_session_index(&planner_b_session_id)
            .expect("second planner session should exist");
        let builder_b_index = inner
            .find_session_index(&builder_b_session_id)
            .expect("second builder session should exist");

        inner.sessions[builder_a_index].runtime = SessionRuntime::Codex(failing_runtime);
        inner.sessions[builder_b_index].session.status = SessionStatus::Active;

        let planner_a_message_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_a_index],
            Message::Text {
                attachments: Vec::new(),
                id: planner_a_message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Implement canvas drop zones.".to_owned(),
                expanded_text: None,
            },
        );
        inner.sessions[planner_a_index].session.status = SessionStatus::Idle;

        let planner_b_message_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_b_index],
            Message::Text {
                attachments: Vec::new(),
                id: planner_b_message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Audit the orchestration editor UI.".to_owned(),
                expanded_text: None,
            },
        );
        inner.sessions[planner_b_index].session.status = SessionStatus::Idle;

        let completion_revision = inner.revision + 1;
        schedule_orchestrator_transitions_for_completed_session(
            &mut inner,
            &HashMap::new(),
            &planner_a_session_id,
            completion_revision,
        );
        schedule_orchestrator_transitions_for_completed_session(
            &mut inner,
            &HashMap::new(),
            &planner_b_session_id,
            completion_revision,
        );
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .resume_pending_orchestrator_transitions()
        .expect("delivery failure in one instance should not block others");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let builder_a = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_a_session_id)
        .expect("first builder session should exist");
    let builder_b = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_b_session_id)
        .expect("second builder session should exist");

    assert_eq!(builder_a.session.status, SessionStatus::Error);
    assert_eq!(builder_b.session.pending_prompts.len(), 1);
    assert!(
        builder_b.session.pending_prompts[0]
            .text
            .contains("Audit the orchestration editor UI.")
    );
    assert!(
        inner
            .orchestrator_instances
            .iter()
            .all(|instance| instance.pending_transitions.is_empty())
    );
}

// Tests that stop session does not schedule orchestrator transitions.
#[test]
fn stop_session_does_not_schedule_orchestrator_transitions() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-transition-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("stop project root should exist");
    let project_id = create_test_project(&state, &project_root, "Stop Transition Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "orchestrator-stop-transition".to_owned(),
        input_tx,
        process: Arc::new(SharedChild::new(test_sleep_child()).unwrap()),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        inner.sessions[planner_index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[planner_index].session.status = SessionStatus::Active;
        inner.sessions[planner_index].active_turn_start_message_count =
            Some(inner.sessions[planner_index].session.messages.len());
        inner.sessions[builder_index].session.status = SessionStatus::Active;
    }

    state
        .stop_session(&planner_session_id)
        .expect("stopping the session should succeed");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let planner = inner
        .sessions
        .iter()
        .find(|record| record.session.id == planner_session_id)
        .expect("planner session should exist");
    let builder = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("builder session should exist");

    assert!(planner.session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn stopped by user."
    )));
    assert!(builder.session.pending_prompts.is_empty());
    assert!(
        inner.orchestrator_instances[0]
            .pending_transitions
            .is_empty()
    );
}

// Tests that fail turn does not schedule orchestrator transitions.
#[test]
fn fail_turn_does_not_schedule_orchestrator_transitions() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-orchestrator-fail-turn-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("fail-turn project root should exist");
    let project_id = create_test_project(&state, &project_root, "Fail Turn Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (runtime, _input_rx) = test_claude_runtime_handle("orchestrator-fail-turn");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        inner.sessions[planner_index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[planner_index].session.status = SessionStatus::Active;
        inner.sessions[planner_index].active_turn_start_message_count =
            Some(inner.sessions[planner_index].session.messages.len());
    }

    state
        .fail_turn_if_runtime_matches(
            &planner_session_id,
            &runtime_token,
            "planner turn failed before completion",
        )
        .expect("turn failure should be handled");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let planner = inner
        .sessions
        .iter()
        .find(|record| record.session.id == planner_session_id)
        .expect("planner session should exist");
    let builder = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("builder session should exist");

    assert!(planner.session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn failed: planner turn failed before completion"
    )));
    assert!(builder.session.pending_prompts.is_empty());
    assert!(
        inner.orchestrator_instances[0]
            .pending_transitions
            .is_empty()
    );
}

// Tests that mark turn error does not schedule orchestrator transitions.
#[test]
fn mark_turn_error_does_not_schedule_orchestrator_transitions() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-orchestrator-mark-error-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("mark-error project root should exist");
    let project_id = create_test_project(&state, &project_root, "Mark Error Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (runtime, _input_rx) = test_claude_runtime_handle("orchestrator-mark-error");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        inner.sessions[planner_index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[planner_index].session.status = SessionStatus::Active;
        inner.sessions[planner_index].active_turn_start_message_count =
            Some(inner.sessions[planner_index].session.messages.len());
    }

    state
        .mark_turn_error_if_runtime_matches(
            &planner_session_id,
            &runtime_token,
            "planner runtime entered an error state",
        )
        .expect("turn error should be handled");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let planner = inner
        .sessions
        .iter()
        .find(|record| record.session.id == planner_session_id)
        .expect("planner session should exist");
    let builder = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("builder session should exist");

    assert_eq!(planner.session.status, SessionStatus::Error);
    assert_eq!(
        planner.session.preview,
        "planner runtime entered an error state"
    );
    assert!(builder.session.pending_prompts.is_empty());
    assert!(
        inner.orchestrator_instances[0]
            .pending_transitions
            .is_empty()
    );
}

// Tests that orchestrator transition uses only messages from the current turn.
#[test]
fn orchestrator_transition_uses_only_messages_from_the_current_turn() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-current-turn-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("current turn project root should exist");
    let project_id = state
        .create_project(CreateProjectRequest {
            name: Some("Current Turn Project".to_owned()),
            root_path: project_root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .expect("project should be created")
        .project_id;
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");

        inner.sessions[builder_index].session.status = SessionStatus::Active;
        let old_message_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_index],
            Message::Text {
                attachments: Vec::new(),
                id: old_message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Old plan from yesterday.".to_owned(),
                expanded_text: None,
            },
        );
        let turn_start = inner.sessions[planner_index].session.messages.len();
        inner.sessions[planner_index].active_turn_start_message_count = Some(turn_start);
        let current_prompt_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_index],
            Message::Text {
                attachments: Vec::new(),
                id: current_prompt_id,
                timestamp: stamp_now(),
                author: Author::You,
                text: "Current task prompt.".to_owned(),
                expanded_text: None,
            },
        );
        inner.sessions[planner_index].session.preview = "Current task prompt.".to_owned();
        inner.sessions[planner_index].session.status = SessionStatus::Idle;

        let completion_revision = inner.revision + 1;
        schedule_orchestrator_transitions_for_completed_session(
            &mut inner,
            &HashMap::new(),
            &planner_session_id,
            completion_revision,
        );
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .resume_pending_orchestrator_transitions()
        .expect("pending transitions should be delivered");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let builder = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("builder session should exist");
    assert_eq!(builder.session.pending_prompts.len(), 1);
    assert!(
        !builder.session.pending_prompts[0]
            .text
            .contains("Old plan from yesterday.")
    );
    assert!(
        builder.session.pending_prompts[0]
            .text
            .contains("Use this plan and implement it:")
    );
}

// Tests that runtime exit does not schedule orchestrator transitions.
#[test]
fn runtime_exit_does_not_schedule_orchestrator_transitions() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-runtime-exit-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("runtime exit project root should exist");
    let project_id = state
        .create_project(CreateProjectRequest {
            name: Some("Runtime Exit Project".to_owned()),
            root_path: project_root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .expect("project should be created")
        .project_id;
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (runtime, _input_rx) = test_claude_runtime_handle("orchestrator-runtime-exit");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        inner.sessions[planner_index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[planner_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index].session.status = SessionStatus::Active;
    }

    state
        .handle_runtime_exit_if_matches(
            &planner_session_id,
            &runtime_token,
            Some("planner runtime crashed"),
        )
        .expect("runtime exit should be handled");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let builder = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("builder session should exist");
    let planner = inner
        .sessions
        .iter()
        .find(|record| record.session.id == planner_session_id)
        .expect("planner session should exist");

    assert!(planner.session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn failed: planner runtime crashed"
    )));
    assert!(builder.session.pending_prompts.is_empty());
    assert!(
        inner.orchestrator_instances[0]
            .pending_transitions
            .is_empty()
    );
}

// Tests that killing a session prunes its orchestrator links.
#[test]
fn killing_a_session_prunes_its_orchestrator_links() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-kill-cleanup-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("kill cleanup project root should exist");
    let project_id = state
        .create_project(CreateProjectRequest {
            name: Some("Kill Cleanup Project".to_owned()),
            root_path: project_root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .expect("project should be created")
        .project_id;
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        let message_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_index],
            Message::Text {
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Plan before kill.".to_owned(),
                expanded_text: None,
            },
        );
        inner.sessions[planner_index].session.status = SessionStatus::Idle;
        let completion_revision = inner.revision + 1;
        schedule_orchestrator_transitions_for_completed_session(
            &mut inner,
            &HashMap::new(),
            &planner_session_id,
            completion_revision,
        );
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .kill_session(&planner_session_id)
        .expect("session should be killed");

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.orchestrator_instances.iter().all(|instance| {
        instance
            .session_instances
            .iter()
            .all(|session| session.session_id != planner_session_id)
    }));
    assert!(inner.orchestrator_instances.iter().all(|instance| {
        instance.pending_transitions.iter().all(|pending| {
            pending.source_session_id != planner_session_id
                && pending.destination_session_id != planner_session_id
        })
    }));
}
