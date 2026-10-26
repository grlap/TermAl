//! Owns the host lifecycle journal's classification, bound and content
//! witnesses (src/host_lifecycle.rs): how the previous instance ended, the
//! size bound with rotation, metadata-only heartbeats taken without the state
//! lock, and the panic record. Journals live under each test's own temp root.
use super::*;

fn journal_directory(state: &AppState, label: &str) -> PathBuf {
    let directory = state.test_temp_root.as_ref().expect("test root").path().join(format!("lifecycle-{label}"));
    fs::create_dir_all(&directory).unwrap();
    directory
}

fn boot(id: &str) -> HostLifecycleRecord {
    HostLifecycleRecord::Boot { at: format!("{id}-boot"), instance_id: id.to_owned(), pid: 7, binary_sha256: None }
}

fn heartbeat(id: &str, at: &str) -> HostLifecycleRecord {
    HostLifecycleRecord::Heartbeat { at: at.to_owned(), instance_id: id.to_owned(),
        counters: HostHeartbeatCounters::default() }
}

fn panic_record(id: &str, at: &str) -> HostLifecycleRecord {
    HostLifecycleRecord::Panic { at: at.to_owned(), instance_id: id.to_owned(), thread: "main".to_owned(),
        location: Some("src/main.rs:1".to_owned()), message: "boom".to_owned() }
}

fn shutdown(id: &str, outcome: HostShutdownOutcome) -> HostLifecycleRecord {
    HostLifecycleRecord::Shutdown { at: format!("{id}-end"), instance_id: id.to_owned(), outcome }
}

#[test]
fn host_lifecycle_classifies_clean_fatal_panic_unclean_and_unknown_ends() {
    use HostShutdownOutcome::{Fatal, Graceful};
    assert_eq!(host_lifecycle_classify(&[]), HostPreviousEnd::Unknown);
    assert_eq!(
        host_lifecycle_classify(&[boot("a"), heartbeat("a", "t1"), shutdown("a", Graceful)]),
        HostPreviousEnd::Shutdown { instance_id: "a".into(), at: "a-end".into(), outcome: Graceful, panics: 0 },
    );
    // A worker-thread panic does not end the process: a later shutdown wins,
    // and the panic is still counted.
    assert_eq!(
        host_lifecycle_classify(&[boot("a"), panic_record("a", "t1"), shutdown("a", Fatal)]),
        HostPreviousEnd::Shutdown { instance_id: "a".into(), at: "a-end".into(), outcome: Fatal, panics: 1 },
    );
    assert_eq!(
        host_lifecycle_classify(&[boot("a"), heartbeat("a", "t1"), panic_record("a", "t2")]),
        HostPreviousEnd::Panic { instance_id: "a".into(), at: "t2".into(), thread: "main".into(),
            location: Some("src/main.rs:1".into()), message: "boom".into(), boot_at: "a-boot".into(),
            last_heartbeat_at: Some("t1".into()), panics: 1 },
    );
    // A caught panic that the process outlived is not its end.
    assert_eq!(
        host_lifecycle_classify(&[boot("a"), panic_record("a", "t1"), heartbeat("a", "t2")]),
        HostPreviousEnd::Unclean { instance_id: "a".into(), boot_at: "a-boot".into(),
            last_heartbeat_at: Some("t2".into()), panics: 1 },
    );
    assert_eq!(
        host_lifecycle_classify(&[boot("a"), heartbeat("a", "t1"), heartbeat("a", "t2")]),
        HostPreviousEnd::Unclean { instance_id: "a".into(), boot_at: "a-boot".into(),
            last_heartbeat_at: Some("t2".into()), panics: 0 },
    );
    assert_eq!(
        host_lifecycle_classify(&[boot("a")]),
        HostPreviousEnd::Unclean { instance_id: "a".into(), boot_at: "a-boot".into(), last_heartbeat_at: None,
            panics: 0 },
    );
    // Only the last boot counts, and only its own instance's records.
    assert_eq!(
        host_lifecycle_classify(&[boot("old"), shutdown("old", Graceful), boot("b"),
            shutdown("old", Graceful), heartbeat("b", "t3")]),
        HostPreviousEnd::Unclean { instance_id: "b".into(), boot_at: "b-boot".into(),
            last_heartbeat_at: Some("t3".into()), panics: 0 },
    );
}

#[test]
fn host_lifecycle_open_classifies_the_previous_boot_and_skips_a_torn_line() {
    let state = test_app_state();
    let directory = journal_directory(&state, "open");
    let first = HostLifecycle::open(directory.clone(), "first".into(), Some("a".repeat(64)));
    assert_eq!(first.previous, HostPreviousEnd::Unknown);
    first.journal.append(&heartbeat("first", "t1")).unwrap();
    // A hard reset can leave a partial last line.
    {
        use std::io::Write;
        let mut file = fs::OpenOptions::new().append(true).open(directory.join(HOST_LIFECYCLE_FILE)).unwrap();
        file.write_all(b"{\"kind\":\"heartbe").unwrap();
    }
    let second = HostLifecycle::open(directory.clone(), "second".into(), None);
    assert!(matches!(&second.previous, HostPreviousEnd::Unclean { instance_id, last_heartbeat_at: Some(at), .. }
        if instance_id == "first" && at == "t1"), "{:?}", second.previous);
    second.record_panic("worker", Some("src/x.rs:9".into()), "late".into());
    let third = HostLifecycle::open(directory.clone(), "third".into(), None);
    assert!(matches!(&third.previous, HostPreviousEnd::Panic { instance_id, thread, .. }
        if instance_id == "second" && thread == "worker"), "{:?}", third.previous);
    third.journal.append(&shutdown("third", HostShutdownOutcome::Graceful)).unwrap();
    let fourth = HostLifecycle::open(directory, "fourth".into(), None);
    assert!(matches!(&fourth.previous, HostPreviousEnd::Shutdown { instance_id, outcome: HostShutdownOutcome::Graceful, .. }
        if instance_id == "third"), "{:?}", fourth.previous);
}

#[test]
fn host_lifecycle_reads_every_whole_line_around_a_torn_multibyte_character() {
    let state = test_app_state();
    let directory = journal_directory(&state, "utf8");
    let journal = HostLifecycleJournal::new(directory.clone());
    journal.append_boot(boot("torn")).unwrap();
    journal.append(&heartbeat("torn", "t1")).unwrap();
    // A reset inside a two-byte character: the line is not valid UTF-8.
    {
        use std::io::Write;
        let mut file = fs::OpenOptions::new().append(true).open(directory.join(HOST_LIFECYCLE_FILE)).unwrap();
        file.write_all(b"{\"kind\":\"panic\",\"message\":\"\xC3").unwrap();
    }
    assert_eq!(journal.read_records(), [boot("torn"), heartbeat("torn", "t1")]);
    let next = HostLifecycle::open(directory, "next".into(), None);
    assert!(matches!(&next.previous, HostPreviousEnd::Unclean { instance_id, last_heartbeat_at: Some(at), .. }
        if instance_id == "torn" && at == "t1"), "{:?}", next.previous);
}

/// Appends wide heartbeats until the instance's boot has rotated out of both
/// files at least once, then the given end record.
fn fill_past_two_rotations(directory: &FsPath, end: Option<HostLifecycleRecord>) -> HostLifecycleJournal {
    let journal = HostLifecycleJournal::new(directory.to_path_buf());
    journal.append_boot(boot("long")).unwrap();
    // A wide record keeps the number of durable appends small.
    let record = heartbeat("long", &"t".repeat(4096));
    let line = serde_json::to_vec(&record).unwrap().len() as u64 + 1;
    for _ in 0..(2 * HOST_LIFECYCLE_MAX_BYTES / line) + 64 {
        journal.append(&record).unwrap();
    }
    if let Some(end) = end {
        journal.append(&end).unwrap();
    }
    journal
}

#[test]
fn host_lifecycle_journal_stays_bounded_and_keeps_the_boot_anchor_across_rotation() {
    let state = test_app_state();
    let directory = journal_directory(&state, "bound");
    let journal = fill_past_two_rotations(&directory, None);
    let current = fs::metadata(directory.join(HOST_LIFECYCLE_FILE)).unwrap().len();
    let previous = fs::metadata(directory.join(HOST_LIFECYCLE_PREVIOUS_FILE)).unwrap().len();
    let wide = serde_json::to_vec(&heartbeat("long", &"t".repeat(4096))).unwrap().len() as u64 + 1;
    let bound = HOST_LIFECYCLE_MAX_BYTES + 2 * wide;
    assert!(current < bound, "current file {current} bytes");
    assert!(previous < bound, "previous file {previous} bytes");
    let names: Vec<_> = fs::read_dir(&directory).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    assert_eq!(names.len(), 2, "exactly the current and one previous file: {names:?}");
    // Each file starts with the long-running instance's boot.
    for name in [HOST_LIFECYCLE_FILE, HOST_LIFECYCLE_PREVIOUS_FILE] {
        let text = fs::read_to_string(directory.join(name)).unwrap();
        let first: HostLifecycleRecord = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(first, boot("long"), "{name}");
    }
    assert!(matches!(host_lifecycle_classify(&journal.read_records()),
        HostPreviousEnd::Unclean { instance_id, boot_at, last_heartbeat_at: Some(_), panics: 0 }
        if instance_id == "long" && boot_at == "long-boot"));

    let directory = journal_directory(&state, "bound-shutdown");
    let journal = fill_past_two_rotations(&directory, Some(shutdown("long", HostShutdownOutcome::Graceful)));
    assert!(matches!(host_lifecycle_classify(&journal.read_records()),
        HostPreviousEnd::Shutdown { instance_id, outcome: HostShutdownOutcome::Graceful, .. } if instance_id == "long"));

    let directory = journal_directory(&state, "bound-panic");
    let journal = fill_past_two_rotations(&directory, Some(panic_record("long", "t9")));
    assert!(matches!(host_lifecycle_classify(&journal.read_records()),
        HostPreviousEnd::Panic { instance_id, at, boot_at, panics: 1, .. }
        if instance_id == "long" && at == "t9" && boot_at == "long-boot"));
}

#[test]
fn host_lifecycle_appends_continue_when_rotation_cannot_rename() {
    let state = test_app_state();
    let directory = journal_directory(&state, "held");
    // A directory where the previous file goes: every rename fails.
    fs::create_dir(directory.join(HOST_LIFECYCLE_PREVIOUS_FILE)).unwrap();
    let journal = fill_past_two_rotations(&directory, Some(shutdown("long", HostShutdownOutcome::Graceful)));
    assert!(fs::metadata(directory.join(HOST_LIFECYCLE_FILE)).unwrap().len() > HOST_LIFECYCLE_MAX_BYTES);
    assert!(matches!(host_lifecycle_classify(&journal.read_records()),
        HostPreviousEnd::Shutdown { instance_id, .. } if instance_id == "long"));
}

#[test]
fn host_lifecycle_heartbeat_is_metadata_only_and_takes_counters_without_the_state_lock() {
    let state = test_app_state();
    let directory = journal_directory(&state, "heartbeat");
    let lifecycle = HostLifecycle::open(directory.clone(), state.server_instance_id.clone(), None);
    // One observed writer tick with payload, as the persist thread records it.
    let scope = state.persist_observer.begin_at(std::time::Instant::now());
    state.persist_observer.update(|tick| {
        tick.counts.metadata_bytes = 100;
        tick.counts.message_bytes = 23;
    });
    scope.finish(true, &PersistTickTimings::default());
    {
        // Held while the heartbeat runs: it must not need the state lock.
        let _held = state.inner.lock().unwrap();
        let counters = HostLifecycle::heartbeat_counters(&state);
        assert_eq!((counters.persist_ticks, counters.persisted_payload_bytes), (1, 123));
        if cfg!(any(windows, target_os = "linux", target_os = "macos")) {
            assert!(counters.working_set_bytes.is_some_and(|bytes| bytes > 0), "{counters:?}");
        }
    }
    // The payload counter is per heartbeat: the next one starts from zero.
    assert_eq!(HostLifecycle::heartbeat_counters(&state).persisted_payload_bytes, 0);
    lifecycle.record_heartbeat(&state).unwrap();
    let text = fs::read_to_string(directory.join(HOST_LIFECYCLE_FILE)).unwrap();
    let last: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    let mut keys: Vec<_> = last["counters"].as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["committedBytes", "longestPersistTickUs", "longestStateHoldAt", "longestStateHoldMs",
        "persistFailedTicks", "persistTicks", "persistedPayloadBytes", "workingSetBytes"]);
    let root = state.test_temp_root.as_ref().unwrap().path().to_string_lossy().into_owned();
    assert!(!text.contains(&root), "no filesystem path is written");
}

#[test]
fn host_lifecycle_state_hold_comes_from_the_existing_over_threshold_report() {
    // Below the threshold nothing is recorded; no hold can reach Duration::MAX.
    let quiet = StateMutex::new_with_diagnostic_reporter(0u8, Duration::MAX, Arc::new(|_| {}));
    drop(quiet.lock().unwrap());
    assert_eq!(quiet.take_longest_hold(), None, "a hold under the threshold records nothing");
    // Every hold reaches a zero threshold, so the record does not depend on timing.
    let reported = Arc::new(Mutex::new(0usize));
    let seen = Arc::clone(&reported);
    // A zero threshold also reports the wait; count only the hold reports.
    let mutex = StateMutex::new_with_diagnostic_reporter(0u8, Duration::ZERO,
        Arc::new(move |diagnostic| if matches!(diagnostic, StateMutexDiagnostic::Held { .. }) {
            *seen.lock().unwrap() += 1;
        }));
    let held = mutex.lock().unwrap();
    let line = line!() - 1;
    drop(held);
    let longest = mutex.take_longest_hold().expect("an over-threshold hold");
    assert_eq!((longest.location.file(), longest.location.line()), (file!(), line));
    assert_eq!(*reported.lock().unwrap(), 1, "the same report path recorded it");
    assert_eq!(mutex.take_longest_hold(), None, "taking resets it");
}

#[test]
fn host_lifecycle_panic_records_carry_no_paths_or_formatted_data() {
    const SENTINEL: &str = "secret-sentinel-7f3a";
    // Literal messages are kept, cut at a character boundary.
    let fixed = std::panic::catch_unwind(|| panic!("fixed text")).unwrap_err();
    assert_eq!(host_lifecycle_panic_message(fixed.as_ref()), "fixed text");
    let long: &'static str = "é".repeat(400).leak();
    let literal = std::panic::catch_unwind(|| std::panic::panic_any(long)).unwrap_err();
    let message = host_lifecycle_panic_message(literal.as_ref());
    assert!(message.len() <= HOST_LIFECYCLE_PANIC_MESSAGE_BYTES && message.len() > 250
        && message.chars().all(|c| c == 'é'), "{message}");
    // Formatted messages can carry runtime data: only their size is kept.
    let runtime = SENTINEL.to_owned();
    let formatted = std::panic::catch_unwind(|| panic!("prompt {runtime} at C:\\Users\\{runtime}")).unwrap_err();
    let message = host_lifecycle_panic_message(formatted.as_ref());
    assert!(!message.contains(SENTINEL) && message.starts_with("formatted message withheld ("), "{message}");
    let opaque = std::panic::catch_unwind(|| std::panic::panic_any(7u8)).unwrap_err();
    assert_eq!(host_lifecycle_panic_message(opaque.as_ref()), "non-string panic payload");

    // Only this crate's relative source path survives; anything else keeps
    // its file name alone.
    assert_eq!(host_lifecycle_location("src/main.rs", 3), "src/main.rs:3");
    assert_eq!(host_lifecycle_location("src\\tests\\mod.rs", 4), "src/tests/mod.rs:4");
    for file in [
        format!("C:\\Users\\{SENTINEL}\\.cargo\\registry\\src\\dep\\lib.rs"),
        format!("/home/{SENTINEL}/.cargo/registry/src/dep/lib.rs"),
        format!("\\\\{SENTINEL}\\share\\lib.rs"),
        format!("../{SENTINEL}/lib.rs"),
    ] {
        assert_eq!(host_lifecycle_location(&file, 5), "lib.rs:5", "{file}");
    }
    assert_eq!(host_lifecycle_location("/rustc/0123abcd/library/core/src/option.rs", 6), "option.rs:6");
    // The same on every host OS: a drive, UNC or escaping step is never kept.
    assert_eq!(host_lifecycle_location("src/../../outside/x.rs", 7), "x.rs:7");
    assert_eq!(host_lifecycle_location("src/C:/x.rs", 8), "x.rs:8");
    assert_eq!(host_lifecycle_location("vendor/dep/lib.rs", 9), "lib.rs:9");
}

#[test]
fn host_lifecycle_thread_records_the_shutdown_without_waiting_out_a_heartbeat() {
    let state = test_app_state();
    let directory = journal_directory(&state, "finish");
    let lifecycle = Arc::new(HostLifecycle::open(directory.clone(), "finishing".into(), None));
    let stop = Arc::new(HostLifecycleStop::default());
    let (running, thread_stop, thread_state) = (Arc::clone(&lifecycle), Arc::clone(&stop), state.clone());
    let thread = std::thread::spawn(move || running.run(&thread_state, &thread_stop));
    let started = std::time::Instant::now();
    HostLifecycleRuntime { stop, thread }.finish(HostShutdownOutcome::Fatal);
    assert!(started.elapsed() < HOST_LIFECYCLE_HEARTBEAT, "finish does not wait out a heartbeat interval");
    let next = HostLifecycle::open(directory, "after".into(), None);
    assert!(matches!(&next.previous, HostPreviousEnd::Shutdown { instance_id, outcome: HostShutdownOutcome::Fatal, panics: 0, .. }
        if instance_id == "finishing"), "{:?}", next.previous);
}

#[test]
fn host_lifecycle_previous_end_serializes_as_documented() {
    let end = HostPreviousEnd::Unclean { instance_id: "a".into(), boot_at: "b".into(), last_heartbeat_at: None,
        panics: 2 };
    assert_eq!(serde_json::to_value(&end).unwrap(),
        json!({ "ended": "unclean", "instanceId": "a", "bootAt": "b", "lastHeartbeatAt": null, "panics": 2 }));
    assert_eq!(serde_json::to_value(HostPreviousEnd::Unknown).unwrap(), json!({ "ended": "unknown" }));
}

#[tokio::test]
async fn host_lifecycle_diagnostics_route_reports_the_journal_and_previous_end() {
    let state = test_app_state();
    let revision = state.inner.lock().unwrap().revision;
    let app = app_router(state.clone());
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder().uri("/api/diagnostics/host-lifecycle").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["serverInstanceId"], state.server_instance_id);
    assert_eq!(body["journal"], HOST_LIFECYCLE_FILE);
    // Tests never start a server's journal thread, so nothing is classified.
    assert!(body["previousInstance"].is_null(), "{body}");
    assert_eq!(state.inner.lock().unwrap().revision, revision);
    // Once classified, the route serves that end.
    let end = HostPreviousEnd::Shutdown { instance_id: "a".into(), at: "t".into(),
        outcome: HostShutdownOutcome::Graceful, panics: 0 };
    let body = host_lifecycle_diagnostics_body("instance", Some(&end));
    assert_eq!(body, json!({ "serverInstanceId": "instance", "journal": HOST_LIFECYCLE_FILE,
        "previousInstance": { "ended": "shutdown", "instanceId": "a", "at": "t", "outcome": "graceful", "panics": 0 } }));
}
