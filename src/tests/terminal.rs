//! Terminal output buffer, terminal-workdir validation, and process-tree
//! streaming tests. Extracted from `tests.rs` so each domain lives in
//! its own sibling module under `tests/`.

use super::*;

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
