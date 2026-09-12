//! Release ownership: a completed archive must not leave an old local slot
//! capable of consuming or deleting the next prompt's registration.
use super::delegation_support::*;
use super::*;

#[test]
fn codex_rpc_timeout_and_transport_are_host_errors_but_rejection_is_bad_request() {
    for (error, expected) in [
        (
            CodexResponseError::Timeout("timeout".to_owned()),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            CodexResponseError::Transport("transport".to_owned()),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            CodexResponseError::JsonRpc("rejected".to_owned()),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (state, input_rx) = test_app_state_with_delegation_codex_runtime("status-mapping");
        let worker = std::thread::spawn(move || {
            let CodexRuntimeCommand::JsonRpcRequest { response_tx, .. } =
                phase_sync::receive(&input_rx, "RPC")
            else {
                panic!("RPC expected")
            };
            response_tx.send(Err(error)).unwrap();
        });
        let result =
            state.perform_codex_json_rpc_request("thread/list", json!({}), Duration::from_secs(1));
        worker.join().unwrap();
        assert_eq!(result.unwrap_err().status, expected);
    }
}

#[test]
fn archive_write_failure_preserves_transport_and_recovers_after_runtime_exit() {
    // Exercise both follow-up admission and the manual recovery action. The
    // fake transport calls the same writer handler production dispatch uses.
    for (automatic, manual_retry) in [(false, false), (false, true), (true, false), (true, true)] {
        let (state, child, delegation, release, input_rx) =
            terminal_child_with_pending_durability();
        let parent = {
            let inner = state.inner.lock().unwrap();
            inner.delegations[inner.find_delegation_index(&delegation).unwrap()]
                .parent_session_id
                .clone()
        };
        let parked = phase_sync::ParkedProcess::spawn();
        state
            .shared_codex_runtime
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .process = parked.process.clone();
        let old = state
            .shared_codex_runtime
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .clone();
        let worker = std::thread::spawn(move || {
            struct BrokenWriter;
            impl Write for BrokenWriter {
                fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "archive never written",
                    ))
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }
            let mut archives = 0;
            while let Ok(CodexRuntimeCommand::JsonRpcRequest {
                method,
                params,
                timeout,
                response_tx,
            }) = input_rx.recv()
            {
                if method == "thread/archive" {
                    archives += 1;
                    assert!(
                        start_shared_codex_rpc_command(
                            &mut BrokenWriter,
                            &Arc::new(Mutex::new(HashMap::new())),
                            "failed-write".to_owned(),
                            &method,
                            params,
                            timeout,
                            response_tx
                        )
                        .is_err()
                    );
                } else {
                    assert_eq!(method, "thread/list");
                    response_tx
                        .send(Ok(json!({"data":[],"nextCursor":null})))
                        .unwrap();
                }
            }
            archives
        });
        let failure = if automatic {
            *release.outcome.lock().unwrap() = None;
            let terminal = {
                let inner = state.inner.lock().unwrap();
                inner.delegations[inner.find_delegation_index(&delegation).unwrap()].clone()
            };
            CodexDelegationReleaseTicket {
                release: release.clone(),
                session_id: child.clone(),
                thread_id: "durability-thread".to_owned(),
                runtime: old.clone(),
                terminal,
                armed: true,
            }
            .start(&state);
            let Some(CodexReleaseOutcome::Ambiguous(detail)) =
                state.wait_for_codex_child_release(&child).unwrap()
            else {
                panic!("write failure must remain ambiguous")
            };
            ApiError::internal(detail)
        } else {
            state.archive_codex_thread(&child).err().unwrap()
        };
        assert!(
            failure.message.contains("archive never written"),
            "typed writer detail must reach caller: {}",
            failure.message
        );
        let before_exit = state.prepare_codex_child_followup(&child).unwrap_err();
        assert_eq!(before_exit.status, StatusCode::CONFLICT);
        let (replacement, replacement_rx, _) = test_shared_codex_runtime("replacement");
        *state.shared_codex_runtime.lock().unwrap() = Some(replacement);
        let inventory = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let worker_inventory = inventory.clone();
        let replacement_worker = std::thread::spawn(move || {
            let mut methods = Vec::new();
            while let Ok(command) = replacement_rx.recv() {
                let CodexRuntimeCommand::JsonRpcRequest {
                    method,
                    params,
                    response_tx,
                    ..
                } = command
                else {
                    assert!(matches!(command, CodexRuntimeCommand::Prompt { .. }));
                    methods.push("prompt".to_owned());
                    continue;
                };
                let response = if method == "thread/list" {
                    json!({"data": if params["archived"] == false && worker_inventory.load(Ordering::SeqCst) { vec![json!({"id":"durability-thread"})] } else { vec![] }, "nextCursor":null})
                } else {
                    json!({})
                };
                methods.push(method);
                response_tx.send(Ok(response)).unwrap();
            }
            methods
        });
        // Slot replacement alone is insufficient: old process is still parked.
        assert!(old.process.try_wait().unwrap().is_none());
        assert!(state.prepare_codex_child_followup(&child).is_err());
        assert!(state.archive_codex_thread(&child).is_err());
        parked.process.kill().unwrap();
        phase_sync::process_exit(&parked.process, "retired original archive runtime");
        // Exit without positive inventory is insufficient too.
        inventory.store(false, Ordering::SeqCst);
        assert!(state.prepare_codex_child_followup(&child).is_err());
        assert!(state.archive_codex_thread(&child).is_err());
        inventory.store(true, Ordering::SeqCst);
        if manual_retry {
            state.archive_codex_thread(&child).unwrap();
            let error = state.prepare_codex_child_followup(&child).unwrap_err();
            assert!(error.message.contains("unarchive"));
            state.unarchive_codex_thread(&child).unwrap();
        }
        state
            .followup_delegation(&parent, &delegation, "Continue after cleanup".to_owned())
            .unwrap();
        drop(old);
        drop(release);
        drop(state);
        assert_eq!(worker.join().unwrap(), 1);
        let methods = replacement_worker.join().unwrap();
        assert_eq!(
            methods
                .iter()
                .filter(|method| *method == "thread/archive")
                .count(),
            usize::from(manual_retry)
        );
        assert!(methods.iter().any(|method| method == "thread/list"));
        assert_eq!(
            methods.iter().filter(|method| *method == "prompt").count(),
            1
        );
    }
}

#[test]
fn archive_timeout_keeps_followup_fenced_until_late_archive_is_confirmed() {
    let (state, _child, delegation, release, input_rx) = terminal_child_with_pending_durability();
    let parent = {
        let inner = state.inner.lock().unwrap();
        inner.delegations[inner.find_delegation_index(&delegation).unwrap()]
            .parent_session_id
            .clone()
    };
    release.finish(CodexReleaseOutcome::Ambiguous(
        "archive timed out, not cancelled".to_owned(),
    ));
    let archived = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_archived = archived.clone();
    let worker = std::thread::spawn(move || {
        let mut prompts = 0;
        let mut restores = 0;
        while let Ok(command) = input_rx.recv() {
            match command {
                CodexRuntimeCommand::JsonRpcRequest {
                    method,
                    params,
                    response_tx,
                    ..
                } => {
                    let response = match method.as_str() {
                        "thread/list" => {
                            let visible =
                                params["archived"] == worker_archived.load(Ordering::SeqCst);
                            Ok(
                                json!({"data": if visible { vec![json!({"id":"durability-thread"})] } else { vec![] }, "nextCursor":null}),
                            )
                        }
                        "thread/unarchive" => {
                            restores += 1;
                            Ok(json!({}))
                        }
                        _ => panic!("unexpected RPC {method}"),
                    };
                    let _ = response_tx.send(response);
                }
                CodexRuntimeCommand::Prompt { .. } => prompts += 1,
                _ => panic!("unexpected command"),
            }
        }
        (prompts, restores)
    });
    let early = state.followup_delegation(&parent, &delegation, "Too early".to_owned());
    // Controlled late server completion, with no notification delivered locally.
    archived.store(true, Ordering::SeqCst);
    let late = state.followup_delegation(&parent, &delegation, "After archive".to_owned());
    let final_outcome = release.outcome.lock().unwrap().clone();
    drop(release);
    drop(state);
    let (prompts, restores) = worker.join().unwrap();
    assert_eq!(
        early.err().map(|error| error.status),
        Some(StatusCode::CONFLICT),
        "an active inventory entry does not finish a timed-out archive"
    );
    assert!(
        late.is_ok(),
        "positive late archived inventory must recover: {:?}",
        late.err().map(|e| e.message)
    );
    assert_eq!((prompts, restores), (1, 1));
    assert_eq!(final_outcome, Some(CodexReleaseOutcome::Restored));
}

#[test]
fn manually_archived_delegation_child_requires_explicit_unarchive() {
    let (state, child, _, release, input_rx) = terminal_child_with_pending_durability();
    // This archive is a user action, not terminal cleanup or its recovery.
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&child).unwrap();
        inner.sessions[index].codex_delegation_release = None;
    }
    let worker = std::thread::spawn(move || {
        let mut methods = Vec::new();
        while let Ok(CodexRuntimeCommand::JsonRpcRequest {
            method,
            response_tx,
            ..
        }) = input_rx.recv()
        {
            methods.push(method);
            let _ = response_tx.send(Ok(json!({})));
        }
        methods
    });
    state.archive_codex_thread(&child).unwrap();
    let result = state.prepare_codex_child_followup(&child);
    let thread_state = state
        .get_session(&child)
        .unwrap()
        .session
        .codex_thread_state;
    drop(release);
    drop(state);
    let methods = worker.join().unwrap();
    assert_eq!(
        thread_state,
        Some(CodexThreadState::Archived),
        "prompt preparation must not undo manual Archive"
    );
    assert!(result.is_err());
    assert_eq!(methods, vec!["thread/archive"]);
}

#[test]
fn archive_response_kind_controls_active_inventory_recovery() {
    for automatic in [false, true] {
        // Identical prose deliberately carries three different transport facts.
        for error in [
            CodexResponseError::JsonRpc("same error".to_owned()),
            CodexResponseError::Timeout("same error".to_owned()),
            CodexResponseError::Transport("same error".to_owned()),
        ] {
            let rejected = matches!(error, CodexResponseError::JsonRpc(_));
            let (state, child, delegation, release, input_rx) =
                terminal_child_with_pending_durability();
            let transport = std::thread::spawn(move || {
                let mut active_probes = 0;
                let mut archives = 0;
                while let Ok(CodexRuntimeCommand::JsonRpcRequest {
                    method,
                    params,
                    response_tx,
                    ..
                }) = input_rx.recv()
                {
                    let response = match method.as_str() {
                        "thread/archive" => {
                            archives += 1;
                            Err(error.clone())
                        }
                        "thread/list" if params["archived"] == false => {
                            active_probes += 1;
                            Ok(json!({"data":[{"id":"durability-thread"}],"nextCursor":null}))
                        }
                        "thread/list" => Ok(json!({"data":[],"nextCursor":null})),
                        _ => panic!("unexpected method {method}"),
                    };
                    let _ = response_tx.send(response);
                }
                (active_probes, archives)
            });
            if automatic {
                *release.outcome.lock().unwrap() = None;
                let terminal = {
                    let inner = state.inner.lock().unwrap();
                    inner.delegations[inner.find_delegation_index(&delegation).unwrap()].clone()
                };
                let runtime = state
                    .shared_codex_runtime
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .clone();
                CodexDelegationReleaseTicket {
                    release: release.clone(),
                    session_id: child.clone(),
                    thread_id: "durability-thread".to_owned(),
                    runtime,
                    terminal,
                    armed: true,
                }
                .start(&state);
            } else {
                assert!(state.archive_codex_thread(&child).is_err());
            }
            let outcome = state.wait_for_codex_child_release(&child).unwrap().unwrap();
            if !rejected {
                assert!(
                    state
                        .archive_codex_thread(&child)
                        .err()
                        .unwrap()
                        .message
                        .contains("still unconfirmed")
                );
            }
            drop(release);
            drop(state);
            let (probes, archives) = transport.join().unwrap();
            assert_eq!(
                archives, 1,
                "ambiguous retry may probe, never enqueue another archive"
            );
            if rejected {
                assert_eq!(outcome, CodexReleaseOutcome::NotArchived);
                assert_eq!(probes, 1);
            } else {
                assert!(
                    matches!(outcome, CodexReleaseOutcome::Ambiguous(_)),
                    "local wait failure must retain ambiguity: {outcome:?}"
                );
                assert_eq!(
                    probes, 0,
                    "active inventory cannot settle an in-flight archive"
                );
            }
        }
    }
}

#[test]
fn archive_inventory_request_pins_all_sources_filters_and_pagination() {
    for archived in [false, true] {
        let (state, input_rx) = test_app_state_with_delegation_codex_runtime("inventory-contract");
        let worker = std::thread::spawn(move || {
            for cursor in [Value::Null, json!("page-2")] {
                let CodexRuntimeCommand::JsonRpcRequest {
                    method,
                    params,
                    timeout,
                    response_tx,
                } = phase_sync::receive(&input_rx, "thread/list page")
                else {
                    panic!("expected inventory request")
                };
                assert_eq!(method, "thread/list");
                // Codex CLI 0.153.4 ThreadListParams: empty modelProviders means
                // all providers; sourceKinds must include noninteractive child
                // kinds explicitly. A null cursor starts pagination.
                assert_eq!(
                    params,
                    json!({"archived":archived,"limit":100,"cursor":cursor,"modelProviders":[],
                    "sourceKinds":["cli","vscode","exec","appServer","subAgent","subAgentReview",
                        "subAgentCompact","subAgentThreadSpawn","subAgentOther","unknown"]})
                );
                assert!(timeout <= CODEX_THREAD_RECONCILIATION_TIMEOUT);
                let first = cursor.is_null();
                response_tx
                    .send(Ok(
                        json!({"data":[{"id":if first {"unrelated-thread"} else {"wanted-thread"}}],
                    "nextCursor":if first {json!("page-2")} else {Value::Null}}),
                    ))
                    .unwrap();
            }
        });
        let found = state.confirm_codex_thread_in_inventory("wanted-thread", archived);
        drop(state);
        worker.join().unwrap();
        assert!(found.unwrap());
    }
}

fn terminal_child_with_pending_durability() -> (
    AppState,
    String,
    String,
    Arc<CodexDelegationRelease>,
    mpsc::Receiver<CodexRuntimeCommand>,
) {
    let (state, input_rx) = test_app_state_with_delegation_codex_runtime("retry-durability");
    let parent = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent,
            CreateDelegationRequest {
                prompt: "Finish".to_owned(),
                title: None,
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Explorer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .unwrap();
    phase_sync::receive(&input_rx, "initial prompt");
    let child = created.delegation.child_session_id;
    finish_delegation_child_with_assistant_text(
        &state,
        &child,
        "## Result\nStatus: completed\n\nSummary:\nRetain this result.",
    );
    state.refresh_delegation_for_child_session(&child).unwrap();
    state
        .set_external_session_id(&child, "durability-thread".to_owned())
        .unwrap();
    let release = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&created.delegation.id).unwrap();
        let release = Arc::new(CodexDelegationRelease {
            undurable_terminal: Mutex::new(Some(inner.delegations[index].clone())),
            terminal_release: true,
            ..Default::default()
        });
        release.finish(CodexReleaseOutcome::NotSent(
            "previous fence failed".to_owned(),
        ));
        let index = inner.find_session_index(&child).unwrap();
        inner.sessions[index].codex_delegation_release = Some(release.clone());
        release
    };
    (state, child, created.delegation.id, release, input_rx)
}

#[test]
fn durability_retry_refreshes_metadata_but_rejects_a_different_or_running_attempt() {
    for change in ["metadata", "attempt", "running"] {
        let (state, _, delegation, release, _) = terminal_child_with_pending_durability();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_delegation_index(&delegation).unwrap();
            inner.delegations[index].title = "Updated terminal metadata".to_owned();
            if change == "attempt" {
                inner.delegations[index].review_result_submission_attempt += 1;
            }
            if change == "running" {
                inner.delegations[index].status = DelegationStatus::Running;
            }
            inner.mark_delegation_mutated(index);
            state.commit_locked(&mut inner).unwrap();
        }
        let result = release.retry_durability(&state);
        if change == "metadata" {
            result.expect(
                "same terminal attempt must fence its current metadata, not the stale snapshot",
            );
            assert!(release.undurable_terminal.lock().unwrap().is_none());
            let disk = load_state(&state.persistence_path).unwrap().unwrap();
            assert_eq!(
                disk.delegations
                    .iter()
                    .find(|d| d.id == delegation)
                    .unwrap()
                    .title,
                "Updated terminal metadata"
            );
        } else {
            assert!(result.unwrap_err().message.contains("attempt changed"));
            assert!(release.undurable_terminal.lock().unwrap().is_some());
        }
    }
}

#[test]
fn manual_archive_preserves_and_retries_a_failed_terminal_fence() {
    let (mut state, child, _, old_release, input_rx) = terminal_child_with_pending_durability();
    let (persist_tx, persist_rx) = mpsc::channel();
    state.persist_tx = persist_tx;
    let persistence = std::thread::spawn(move || {
        let mut fences = 0;
        while let Ok(request) = persist_rx.recv() {
            if let PersistRequest::Fence(fence) = request {
                fences += 1;
                fence.finish(Err(PersistFenceError::WriteFailed(
                    "storage still unavailable".to_owned(),
                )));
            }
        }
        fences
    });
    let transport = std::thread::spawn(move || {
        let mut commands = Vec::new();
        while let Ok(command) = input_rx.recv() {
            if let CodexRuntimeCommand::JsonRpcRequest {
                method,
                response_tx,
                ..
            } = command
            {
                commands.push(method);
                let _ = response_tx.send(Ok(json!({})));
            }
        }
        commands
    });
    let result = state.archive_codex_thread(&child);
    let retains_obligation = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&child).unwrap()]
            .codex_delegation_release
            .as_ref()
            .unwrap()
            .undurable_terminal
            .lock()
            .unwrap()
            .is_some()
    };
    drop(old_release);
    drop(state);
    let fences = persistence.join().unwrap();
    let commands = transport.join().unwrap();
    assert!(
        result
            .err()
            .expect("manual Archive must not bypass failed durability")
            .message
            .contains("storage still unavailable")
    );
    assert_eq!(fences, 1);
    assert!(retains_obligation);
    assert!(
        commands.is_empty(),
        "failed terminal fence must precede any manual archive RPC"
    );
}

#[test]
fn release_wait_budget_covers_the_serial_operations_and_commit_headroom() {
    let serial = CODEX_CHILD_RESULT_FENCE_TIMEOUT
        + CODEX_CHILD_ARCHIVE_REPLY_TIMEOUT
        + CODEX_THREAD_RECONCILIATION_REPLY_TIMEOUT * 2;
    assert!(CODEX_CHILD_RELEASE_WAIT_TIMEOUT >= serial + Duration::from_secs(10));
    assert!(CODEX_CHILD_ARCHIVE_REPLY_TIMEOUT > CODEX_CHILD_ARCHIVE_RPC_TIMEOUT);
}

#[test]
fn deterministic_archive_failure_allows_followup_after_positive_active_inventory() {
    for (automatic, late_inventory) in [(false, false), (true, false), (true, true)] {
        let (state, input_rx) = test_app_state_with_delegation_codex_runtime("refused-archive");
        let parent = test_session_id(&state, Agent::Codex);
        let created = state
            .create_read_only_delegation(
                &parent,
                CreateDelegationRequest {
                    prompt: "Finish".to_owned(),
                    title: None,
                    cwd: None,
                    agent: Some(Agent::Codex),
                    model: None,
                    mode: Some(DelegationMode::Explorer),
                    write_policy: Some(DelegationWritePolicy::ReadOnly),
                },
            )
            .unwrap();
        phase_sync::receive(&input_rx, "initial prompt");
        let child = created.delegation.child_session_id;
        state
            .set_external_session_id(&child, "active-after-refusal".to_owned())
            .unwrap();
        if !automatic {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&child).unwrap();
            inner.sessions[index].runtime = SessionRuntime::None;
        }
        let (prompt_tx, prompt_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut active_probes = 0;
            while let Ok(command) = input_rx.recv() {
                match command {
                    CodexRuntimeCommand::JsonRpcRequest {
                        method,
                        params,
                        response_tx,
                        ..
                    } => {
                        let response = match method.as_str() {
                            "thread/archive" => Err(CodexResponseError::JsonRpc(
                                "fixture deterministic archive refusal".to_owned(),
                            )),
                            "thread/list" if params["archived"] == false => {
                                active_probes += 1;
                                if late_inventory && active_probes == 1 {
                                    Err(CodexResponseError::Transport(
                                        "inventory temporarily unavailable".to_owned(),
                                    ))
                                } else {
                                    Ok(
                                        json!({"data":[{"id":"active-after-refusal"}],"nextCursor":null}),
                                    )
                                }
                            }
                            "thread/list" => Ok(json!({"data":[],"nextCursor":null})),
                            other => panic!("unexpected RPC {other}"),
                        };
                        let _ = response_tx.send(response);
                    }
                    CodexRuntimeCommand::Prompt { .. } => {
                        prompt_tx.send(()).unwrap();
                    }
                    _ => panic!("unexpected command"),
                }
            }
        });
        finish_delegation_child_with_assistant_text(
            &state,
            &child,
            "## Result\nStatus: completed\n\nSummary:\nDone.",
        );
        state.refresh_delegation_for_child_session(&child).unwrap();
        if automatic {
            state.wait_for_codex_child_release(&child).unwrap();
        } else {
            assert!(
                state
                    .archive_codex_thread(&child)
                    .err()
                    .unwrap()
                    .message
                    .contains("deterministic")
            );
        }
        let result =
            state.followup_delegation(&parent, &created.delegation.id, "Continue".to_owned());
        drop(state);
        worker.join().unwrap();
        assert!(
            result.is_ok(),
            "positive non-archived identity must allow followup: {:?}",
            result.err().map(|e| e.message)
        );
        assert_eq!(prompt_rx.try_iter().count(), 1);
    }
}

#[test]
fn ordinary_codex_session_does_not_wait_for_child_release() {
    let state = test_app_state();
    let session = test_session_id(&state, Agent::Codex);
    let release = Arc::new(CodexDelegationRelease::default());
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].codex_delegation_release = Some(release.clone());
    }
    // Both the resume mutex and pending outcome are deliberately unavailable.
    // Eligibility must be checked before touching either synchronization point.
    let held = release.resume.lock().unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        done_tx
            .send(state.prepare_codex_child_followup(&session))
            .unwrap();
    });
    let result =
        phase_sync::receive_before_cleanup(&done_rx, "ordinary prompt bypasses child wait");
    drop(held);
    release.finish(CodexReleaseOutcome::Archived);
    worker.join().unwrap();
    result.unwrap().unwrap();
}

#[test]
fn not_sent_release_retains_cleanup_and_durability_on_drop_or_enqueue_failure() {
    for failure in ["drop", "queue", "runtime"] {
        let (state, input_rx) = test_app_state_with_delegation_codex_runtime("not-sent-recovery");
        let parent = test_session_id(&state, Agent::Codex);
        let created = state
            .create_read_only_delegation(
                &parent,
                CreateDelegationRequest {
                    prompt: "Finish.".to_owned(),
                    title: None,
                    cwd: None,
                    agent: Some(Agent::Codex),
                    model: None,
                    mode: Some(DelegationMode::Explorer),
                    write_policy: Some(DelegationWritePolicy::ReadOnly),
                },
            )
            .unwrap();
        phase_sync::receive(&input_rx, "initial prompt");
        let child = created.delegation.child_session_id;
        state
            .set_external_session_id(&child, "not-sent-thread".to_owned())
            .unwrap();
        finish_delegation_child_with_assistant_text(
            &state,
            &child,
            "## Result\nStatus: completed\n\nSummary:\nRetained result.",
        );
        let runtime = state
            .shared_codex_runtime
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .clone();
        let mut sessions = runtime.sessions.lock().unwrap();
        sessions.insert(
            child.clone(),
            SharedCodexSessionState {
                thread_id: Some("not-sent-thread".to_owned()),
                ..Default::default()
            },
        );
        runtime
            .thread_sessions
            .lock()
            .unwrap()
            .insert("not-sent-thread".to_owned(), child.clone());
        let ticket = {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_delegation_index(&created.delegation.id).unwrap();
            refresh_delegation_from_child_locked(&mut inner, index);
            let mut detached = detach_terminal_delegation_child_runtime_locked(&mut inner, index);
            state.commit_locked(&mut inner).unwrap();
            detached.codex_release.take().unwrap()
        };
        if failure == "drop" {
            drop(ticket);
            assert!(
                state
                    .prepare_codex_child_followup(&child)
                    .unwrap_err()
                    .message
                    .contains("local detachment"),
                "a dropped ticket cannot bypass the reader's old local slot"
            );
        } else {
            if failure == "queue" {
                drop(input_rx);
            } else {
                state.shared_codex_runtime.lock().unwrap().take();
            }
            ticket.start(&state);
        }
        drop(sessions);
        assert!(matches!(
            state.wait_for_codex_child_release(&child).unwrap(),
            Some(CodexReleaseOutcome::NotSent(_))
        ));
        state.prepare_codex_child_followup(&child).unwrap();
        assert!(!runtime.sessions.lock().unwrap().contains_key(&child));
        assert!(
            !runtime
                .thread_sessions
                .lock()
                .unwrap()
                .contains_key("not-sent-thread")
        );
        assert!(
            load_state(&state.persistence_path)
                .unwrap()
                .unwrap()
                .delegations
                .iter()
                .any(|record| record.id == created.delegation.id && record.result.is_some())
        );
    }
}

#[test]
fn release_worker_detaches_before_archive_and_immediate_followup() {
    let (state, input_rx) = test_app_state_with_delegation_codex_runtime("release-detach-owner");
    let parent = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent,
            CreateDelegationRequest {
                prompt: "Finish.".to_owned(),
                title: None,
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Explorer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .unwrap();
    phase_sync::receive(&input_rx, "initial prompt");
    let child = created.delegation.child_session_id;
    state
        .set_external_session_id(&child, "released-thread".to_owned())
        .unwrap();
    finish_delegation_child_with_assistant_text(
        &state,
        &child,
        "## Result\nStatus: completed\n\nSummary:\nDone.",
    );
    let runtime = state
        .shared_codex_runtime
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .clone();
    // Emulate the stdout reader owning its local map during completion. Use
    // the real ticket reservation seam, without an independent detach worker.
    let mut sessions = runtime.sessions.lock().unwrap();
    sessions.insert(
        child.clone(),
        SharedCodexSessionState {
            thread_id: Some("released-thread".to_owned()),
            ..Default::default()
        },
    );
    runtime
        .thread_sessions
        .lock()
        .unwrap()
        .insert("released-thread".to_owned(), child.clone());
    let ticket = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&created.delegation.id).unwrap();
        refresh_delegation_from_child_locked(&mut inner, index);
        let mut detached = detach_terminal_delegation_child_runtime_locked(&mut inner, index);
        assert!(
            detached.did_mutate(),
            "transferring the runtime to a release ticket still requires a commit"
        );
        assert!(
            detached.runtime.is_none(),
            "the ticket must be the only local cleanup owner"
        );
        state.commit_locked(&mut inner).unwrap();
        detached.codex_release.take().unwrap()
    };
    ticket.start(&state);
    drop(sessions); // reader can now progress; the worker owns all cleanup.
    let CodexRuntimeCommand::JsonRpcRequest {
        method,
        response_tx,
        ..
    } = phase_sync::receive(&input_rx, "archive after detach")
    else {
        panic!("expected archive")
    };
    assert_eq!(method, "thread/archive");
    let stale_slot = runtime.sessions.lock().unwrap().contains_key(&child);
    let stale_mapping = runtime
        .thread_sessions
        .lock()
        .unwrap()
        .contains_key("released-thread");
    response_tx.send(Ok(json!({}))).unwrap();
    state.wait_for_codex_child_release(&child).unwrap();
    assert!(
        !stale_slot,
        "archive admission must follow removal of the old local session"
    );
    assert!(
        !stale_mapping,
        "archive admission must follow removal of its thread mapping"
    );

    let following = state.clone();
    let followup = std::thread::spawn(move || {
        following.followup_delegation(&parent, &created.delegation.id, "Continue.".to_owned())
    });
    let CodexRuntimeCommand::JsonRpcRequest {
        method,
        response_tx,
        ..
    } = phase_sync::receive(&input_rx, "unarchive for immediate followup")
    else {
        panic!("expected unarchive")
    };
    assert_eq!(method, "thread/unarchive");
    response_tx.send(Ok(json!({}))).unwrap();
    assert!(matches!(
        phase_sync::receive(&input_rx, "followup prompt"),
        CodexRuntimeCommand::Prompt { .. }
    ));
    followup.join().unwrap().unwrap();
    assert!(
        !runtime.sessions.lock().unwrap().contains_key(&child),
        "next prompt must resume, not reuse the unloaded thread slot"
    );
    runtime.sessions.lock().unwrap().insert(
        child.clone(),
        SharedCodexSessionState {
            thread_id: Some("followup-registration".to_owned()),
            ..Default::default()
        },
    );
    state.wait_for_codex_child_release(&child).unwrap();
    assert_eq!(
        runtime
            .sessions
            .lock()
            .unwrap()
            .get(&child)
            .unwrap()
            .thread_id
            .as_deref(),
        Some("followup-registration"),
        "completed cleanup must not detach the new registration"
    );
}
