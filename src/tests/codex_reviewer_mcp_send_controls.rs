// Owns blocked approved-send I/O and watchdog recovery at the actual handler.
// The channel-stalled sink and parked process are local, not provider evidence.
use super::controls::{next_command, ready_page, respond_start};
use super::super::phase_sync::receive_before_cleanup;
use super::*;

struct StalledSink {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

impl Write for StalledSink {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        let _ = self.entered.send(());
        let _ = self.release.recv();
        Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "local stalled sink released"))
    }

    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

struct ReleaseStalledSink(mpsc::Sender<()>);

impl Drop for ReleaseStalledSink {
    fn drop(&mut self) { let _ = self.0.send(()); }
}

#[test]
fn codex_reviewer_mcp_blocked_approved_send_allows_status_and_watchdog_kill() {
    let mut fixture = ReviewerFixture::new("blocked-approved-review-send");
    let parked = super::super::phase_sync::ParkedProcess::spawn();
    fixture.process = parked.process.clone();
    fixture.runtime.process = parked.process.clone();
    install_single_wire_codex_fixture(&fixture.state, None);
    let profile;
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let i = inner.find_session_index(&fixture.child).unwrap();
        profile = inner.sessions[i].shared_codex_profile();
        let SessionRuntime::Codex(handle) = &mut inner.sessions[i].runtime else {
            panic!("fixture must keep its exact Codex runtime");
        };
        handle.process = parked.process.clone();
        handle.shared_session.as_mut().expect("shared reviewer attachment").runtime.process = parked.process.clone();
    }
    *fixture.state.shared_codex_runtime_slot(profile).lock().unwrap() = Some(fixture.runtime.clone());
    let written = fixture.start();
    respond_start(&fixture, &written, ready_page());
    let CodexRuntimeCommand::ReviewerMcpReady { scope, command, watchdog } = next_command(&fixture) else {
        panic!("the real startup worker must authorize this continuation");
    };
    let activity: SharedCodexStdinActivityState = Arc::new(Mutex::new(None));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release = ReleaseStalledSink(release_tx);
    let state = fixture.state.clone();
    let runtime = fixture.runtime.clone();
    let pending = fixture.pending.clone();
    let child = fixture.child.clone();
    let observed_activity = activity.clone();
    let (writer_done_tx, writer_done_rx) = mpsc::channel();
    let writer_thread = std::thread::spawn(move || {
        let mut writer = SharedCodexWatchedWriter::new(StalledSink {
            entered: entered_tx, release: release_rx,
        }, observed_activity);
        let result = handle_shared_codex_start_turn_inner(
            &mut writer, &pending, &state, &runtime.runtime_id, &runtime.sessions,
            &runtime.thread_sessions, None, &child, REVIEW_THREAD, watchdog, command, Some(&scope),
        );
        let _ = writer_done_tx.send(result);
    });
    entered_rx.recv_timeout(Duration::from_secs(3)).expect("approved send must reach actual watched I/O");
    let shared_unlocked = fixture.runtime.sessions.try_lock().is_ok();
    let state = fixture.state.clone();
    let (status_tx, status_rx) = mpsc::channel();
    let status_thread = std::thread::spawn(move || {
        let _ = status_tx.send(state.full_snapshot());
    });
    // Capture before releasing the sink or starting cleanup; this is a
    // concurrent status operation, not evidence inferred from a later snapshot.
    let status = status_rx.recv_timeout(Duration::from_secs(3));
    let (stop_tx, stop_rx) = mpsc::channel();
    spawn_shared_codex_stdin_watchdog(
        &fixture.state, &fixture.runtime.runtime_id, parked.process.clone(), &activity,
        stop_rx, Duration::from_millis(20), Duration::from_millis(5),
    ).unwrap();
    let process = parked.process.clone();
    let (exited_tx, exited_rx) = mpsc::channel();
    let process_waiter = std::thread::spawn(move || { let _ = exited_tx.send(process.wait()); });
    // Observe shutdown before exit; the parked fixture deliberately ignores
    // it. The shared phase guard detects a missing publication, not a latency
    // promise that undercuts the production three-second graceful allowance.
    let shutdown_before_release = receive_before_cleanup(&fixture.input_rx, "watchdog queued shutdown while I/O blocked");
    // Only the watchdog can kill the still-parked process before sink release.
    let exited_before_release = receive_before_cleanup(&exited_rx, "watchdog hard kill while I/O blocked");
    drop(release);
    let writer_result = writer_done_rx.recv_timeout(Duration::from_secs(3)).expect("released writer must finish");
    writer_thread.join().unwrap();
    status_thread.join().unwrap();
    // Emergency reap keeps a failed control from stranding fixture threads;
    // it cannot turn the already-captured negative observation into a pass.
    if exited_before_release.is_err() { let _ = parked.process.kill(); }
    process_waiter.join().unwrap();
    let _ = stop_tx.send(());
    assert!(writer_result.unwrap_err().to_string().contains("local stalled sink released"));
    assert!(shared_unlocked, "approved pipe I/O must not own the shared-session lock");
    let snapshot = status.expect("status must acquire the global state lock while I/O is blocked");
    assert_eq!(snapshot.sessions.iter().find(|s| s.id == fixture.child).unwrap().status, SessionStatus::Active);
    assert!(matches!(shutdown_before_release.expect("watchdog must reach shutdown before I/O release"),
        CodexRuntimeCommand::JsonRpcNotification { method } if method == "shutdown"));
    assert!(exited_before_release.expect("watchdog cleanup must reach process kill before I/O release").is_ok());
    assert!(fixture.state.shared_codex_runtime_slot(profile).lock().unwrap().is_none());
    let inner = fixture.state.inner.lock().unwrap();
    let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
    assert_eq!(child.session.status, SessionStatus::Error);
}
