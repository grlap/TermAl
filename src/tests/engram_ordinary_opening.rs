//! Opening preparation must not occupy the shared runtime reader with I/O.
use super::*;

struct ParkedAuthority {
    gate: (Mutex<bool>, Condvar),
    entered: Mutex<Option<mpsc::Sender<()>>>,
    reader: Mutex<Option<std::thread::ThreadId>>,
    reader_calls: std::sync::atomic::AtomicUsize,
}

impl ParkedAuthority {
    fn wait(&self) {
        if self.reader.lock().unwrap().as_ref() == Some(&std::thread::current().id()) {
            self.reader_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        if let Some(entered) = self.entered.lock().unwrap().take() {
            let _ = entered.send(());
        }
        let released = self.gate.0.lock().unwrap();
        drop(
            self.gate
                .1
                .wait_while(released, |released| !*released)
                .unwrap(),
        );
    }
}

impl EngramControlTransport for ParkedAuthority {
    fn read_observation_policy(
        &self,
        _: &EngramConnectionConfig,
        _: &EngramAuthorityStoreKey,
        _: Duration,
        _: &FsPath,
    ) -> Result<EngramObservationPolicyBasis, EngramTransportError> {
        self.wait();
        Err(EngramTransportError::deadline(
            "parked policy answer released",
        ))
    }

    fn request(
        &self,
        _: &EngramConnectionConfig,
        _: &EngramControlRequest,
        _: Duration,
    ) -> Result<Value, EngramTransportError> {
        self.wait();
        Err(EngramTransportError::deadline(
            "parked authority answer released",
        ))
    }

    fn read_work_binding(
        &self,
        _: &EngramConnectionConfig,
        _: EngramBindingPreference<'_>,
        _: Duration,
    ) -> Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.wait();
        Err(EngramTransportError::deadline(
            "parked binding answer released",
        ))
    }

    fn shutdown_session(&self, _: &str) {}
}

struct ReleaseAuthority(Arc<ParkedAuthority>);

impl Drop for ReleaseAuthority {
    fn drop(&mut self) {
        *self.0.gate.0.lock().unwrap() = true;
        self.0.gate.1.notify_all();
    }
}

#[test]
fn queued_opening_and_shared_reader_progress_before_engram_answer() {
    let mut turn = EOCheckedTurn::idle("opening-shared-reader", true);
    let process = phase_sync::ParkedProcess::spawn();
    let (runtime, commands, _) =
        test_shared_codex_runtime_with_process("opening-shared-runtime", process.process.clone());
    install_single_wire_codex_fixture(&turn.state, Some(runtime.clone()));
    turn._runtime_rx = Arc::new(Mutex::new(commands));
    turn.begin("The first ordinary prompt.");
    let generation = turn.record(|record| record.active_turn_generation);
    turn.record_mut(|record| set_record_external_session_id(record, Some("opening-thread".into())));
    runtime.sessions.lock().unwrap().insert(
        turn.session_id.clone(),
        SharedCodexSessionState {
            thread_id: Some("opening-thread".into()),
            turn_id: Some("opening-turn".into()),
            active_turn_generation: Some(generation),
            turn_started: true,
            ..Default::default()
        },
    );
    runtime
        .thread_sessions
        .lock()
        .unwrap()
        .insert("opening-thread".into(), turn.session_id.clone());
    assert!(matches!(
        turn.state
            .dispatch_turn(
                &turn.session_id,
                SendMessageRequest {
                    text: "Queued ordinary successor.".into(),
                    expanded_text: None,
                    attachments: Vec::new(),
                    source_session_id: None,
                    source_mailbox: None,
                }
            )
            .unwrap(),
        DispatchTurnResult::Queued
    ));
    let (entered_tx, entered_rx) = mpsc::channel();
    let authority = Arc::new(ParkedAuthority {
        gate: (Mutex::new(false), Condvar::new()),
        entered: Mutex::new(Some(entered_tx)),
        reader: Mutex::new(None),
        reader_calls: std::sync::atomic::AtomicUsize::new(0),
    });
    turn.state.install_test_engram_transport(authority.clone());
    let target = AppState::engram_source_root_target_locked(
        &turn.state.inner.lock().unwrap(),
        &turn.session_id,
    )
    .unwrap()
    .unwrap();
    let pending: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let (reply_tx, reply_rx) = mpsc::channel();
    pending
        .lock()
        .unwrap()
        .insert("unrelated-opening-reply".into(), reply_tx);
    std::thread::scope(|scope| {
        // This guard releases every parked call before scoped joins, including
        // when an assertion detects that the runtime reader was blocked.
        let _release = ReleaseAuthority(authority.clone());
        let state = &turn.state;
        let canonical_reader = scope.spawn(move || {
            state.read_engram_authority_fact_until(
                &target,
                target.work_binding.as_ref().unwrap(),
                state.engram_budget_clock().now() + DEADLOCK_GUARD,
            )
        });
        receive(
            &entered_rx,
            "an Engram answer is parked before terminal delivery",
        );
        let (frames_tx, frames_rx) = mpsc::channel::<Value>();
        let (completed_tx, completed_rx) = mpsc::channel();
        let reader_authority = &authority;
        let reader_state = &turn.state;
        let reader_runtime = &runtime;
        let reader_pending = &pending;
        let reader = scope.spawn(move || {
            *reader_authority.reader.lock().unwrap() = Some(std::thread::current().id());
            for frame in frames_rx {
                handle_shared_codex_app_server_message(
                    &frame,
                    reader_state,
                    &reader_runtime.runtime_id,
                    reader_pending,
                    &reader_runtime.sessions,
                    &reader_runtime.thread_sessions,
                    &reader_runtime.input_tx,
                )
                .unwrap();
                if frame["method"] == "turn/completed" {
                    completed_tx.send(()).unwrap();
                }
            }
        });
        frames_tx
            .send(json!({"method":"turn/completed", "params": {
                "threadId":"opening-thread", "turn":{"id":"opening-turn", "status":"completed"}
            }}))
            .unwrap();
        frames_tx
            .send(json!({"id":"unrelated-opening-reply", "result":{"routed":true}}))
            .unwrap();
        let command = receive(
            &turn._runtime_rx.lock().unwrap(),
            "queued provider enqueue before Engram answer",
        );
        assert!(
            matches!(command, CodexRuntimeCommand::Prompt { session_id, command }
            if session_id == turn.session_id && command.active_turn_generation == generation + 1
                && command.prompt.ends_with("Queued ordinary successor."))
        );
        receive(
            &completed_rx,
            "shared reader returns from terminal callback",
        );
        assert_eq!(
            receive(&reply_rx, "same reader routes an unrelated response").unwrap(),
            json!({"routed":true})
        );
        assert!(
            !*authority.gate.0.lock().unwrap(),
            "progress preceded the parked answer"
        );
        assert_eq!(
            authority
                .reader_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0,
            "neither terminal drain nor provider handoff calls Engram on the reader"
        );
        turn.record(|record| {
            assert!(
                record
                    .engram
                    .unadmitted_active_turn
                    .as_ref()
                    .unwrap()
                    .opening_prepared
            );
            assert_eq!(
                record.engram.observed_opening().unwrap().provenance,
                EngramRootCapture::Unconfirmed
            );
        });
        drop(frames_tx);
        reader.join().unwrap();
        drop(_release);
        assert!(
            canonical_reader.join().unwrap().is_err(),
            "the parked failure remains a failure"
        );
    });
    turn.state.trigger_shutdown_signal();
    turn.state.shutdown_persist_blocking();
}

#[test]
fn later_authority_confirmation_cannot_relabel_the_prepared_opening() {
    let turn = EOCheckedTurn::idle("opening-late-confirmation", true);
    let prompt = turn.begin_prompt("Keep this uncertain start.");
    assert!(prompt.prompt.contains("source root is unconfirmed"));
    let opening = turn.record(|record| record.engram.observed_opening().cloned());
    turn.acknowledge_unnamed_root();
    let (token, generation) = turn.record(|record| {
        (
            record.runtime.runtime_token().unwrap(),
            record.active_turn_generation,
        )
    });
    turn.state
        .prepare_ordinary_opening_root_before_handoff(&turn.session_id, &token, generation);
    assert_eq!(
        turn.record(|record| record.engram.observed_opening().cloned()),
        opening
    );
    assert_eq!(
        turn.state.engram_turn_root_capture(&turn.session_id),
        EngramRootCapture::Unconfirmed
    );
    assert!(turn.record(|record| {
        record
            .engram
            .unadmitted_active_turn
            .as_ref()
            .unwrap()
            .source_attribution_uncertain
    }));
    let report = turn.finish();
    assert!(
        report["occurrence"]["source_change"].is_null(),
        "a later confirmation cannot attribute source evidence to the uncertain start: {report}"
    );
    assert_ordinary_observation_has_no_native_credit(&turn, &report);
    turn.state.trigger_shutdown_signal();
    turn.state.shutdown_persist_blocking();
}

#[test]
fn local_opening_sweeps_checks_in_the_acknowledged_named_worktree() {
    let turn = EOCheckedTurn::idle("opening-local-overlap", true);
    let named = turn.root.join(".worktrees").join("opening-overlap");
    run_git_test_command(
        &turn.root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "opening-overlap",
            named.to_str().unwrap(),
        ],
    );
    let work = turn.record(|record| record.engram.work_binding.as_ref().unwrap().work_id.clone());
    turn.state
        .name_engram_source_root(
            &turn.session_id,
            EngramSourceRootRequest {
                work,
                path: Some(Some(named.to_string_lossy().into_owned())),
            },
        )
        .unwrap();
    let other = other_session_in_worktree(&turn);
    let pending = Arc::new(EngramBasisCapture::default());
    let mut check = finished_check(0, pending.clone());
    check.target.root = named.clone();
    check.target.directory = named;
    {
        let mut inner = turn.state.inner.lock().unwrap();
        inner
            .session_mut(&other)
            .unwrap()
            .engram
            .active_turn_checks
            .push(check);
    }
    let dispatch = match turn
        .state
        .dispatch_turn(
            &turn.session_id,
            SendMessageRequest {
                text: "Start work in the acknowledged root.".into(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .unwrap()
    {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("idle ordinary owner"),
    };
    let overlapped = || {
        let inner = turn.state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&other).unwrap()]
            .engram
            .active_turn_checks[0]
            .overlapped
    };
    assert!(
        !overlapped(),
        "the first turn-start sweep preceded installation of this named root"
    );
    let calls = turn.transport.requests().len();
    deliver_turn_dispatch(&turn.state, dispatch).unwrap();
    assert!(matches!(
        receive(
            &turn._runtime_rx.lock().unwrap(),
            "named-root provider enqueue"
        ),
        CodexRuntimeCommand::Prompt { .. }
    ));
    assert_eq!(
        turn.transport.requests().len(),
        calls,
        "the overlap sweep is entirely local"
    );
    assert!(
        overlapped(),
        "preparation must preserve the post-install named-root overlap sweep"
    );
    pending.finish(None);
    turn.state.trigger_shutdown_signal();
    turn.state.shutdown_persist_blocking();
}
