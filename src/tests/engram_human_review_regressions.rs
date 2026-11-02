// Route and failed-promotion boundaries for explicit Human dispatch. No live
// state, authority transport or provider is used; original control debt stays.
use super::*;
use crate::tests::delegation_support::install_required_review_delegation;

fn ordinary_selection_fixture() -> (AppState, String, mpsc::Receiver<CodexRuntimeCommand>) {
    let (state, receiver) =
        test_app_state_with_delegation_codex_runtime("ordinary-selected-resume");
    let session = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.session.status = SessionStatus::Idle;
        record.set_auto_dispatch_blocked(true);
        queue_prompt_on_record(
            record,
            PendingPrompt {
                id: "original-ordinary-head".into(),
                timestamp: stamp_now(),
                text: "ordinary original payload".into(),
                expanded_text: None,
                source: None,
                attachments: vec![],
                engram_interrupted: false,
                is_engram_retained: false,
            },
            vec![],
        );
        state.commit_locked(&mut inner).unwrap();
    }
    (state, session, receiver)
}

fn selected_head(state: &AppState, session: &str) -> HumanPromptSelection {
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    HumanPromptSelection {
        prompt: projected_pending_prompts(record)[0].clone(),
        session_mutation_stamp: record.mutation_stamp,
    }
}

#[test]
fn human_prompt_selected_unknown_head_cannot_authorize_a_distinct_successor() {
    let (state, session, receiver) = ordinary_selection_fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        record.queued_prompts[0].source = QueuedPromptSource::Human;
        let mut next = record.queued_prompts[0].clone();
        next.pending_prompt.id = "independent-human".into();
        next.pending_prompt.text = "independent content".into();
        record.queued_prompts[0].engram_ordinary_handoff = true;
        record.queued_prompts.push_back(next);
        sync_pending_prompts(record);
        inner.stamp_session_at_index(index);
        state.commit_locked(&mut inner).unwrap();
    }
    let error = state
        .resume_selected_human_prompt(&session, selected_head(&state, &session))
        .err()
        .expect("unknown selection owns no successor");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.queued_prompts.len(), 2);
    assert!(record.queued_prompts[0].engram_ordinary_handoff);
    assert_eq!(
        record.queued_prompts[1].pending_prompt.id,
        "independent-human"
    );
    assert!(record.orchestrator_auto_dispatch_blocked);
}

#[test]
fn human_prompt_selected_resume_never_starts_a_concurrent_replacement() {
    for phase in ["before_resume", "before_reset", "before_promotion"] {
        for replacement in ["human", "ordinary", "edited", "stop"] {
            let (state, session, receiver) = ordinary_selection_fixture();
            {
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&session).unwrap();
                let record = &mut inner.sessions[index];
                record.queued_prompts[0].source = QueuedPromptSource::Human;
                if matches!(replacement, "human" | "ordinary") {
                    let mut next = record.queued_prompts[0].clone();
                    next.pending_prompt.id = "replacement-head".into();
                    next.pending_prompt.text = "replacement payload".into();
                    if replacement == "ordinary" {
                        next.source = QueuedPromptSource::User;
                    }
                    record.queued_prompts.push_back(next);
                }
                sync_pending_prompts(record);
                inner.stamp_session_at_index(index);
                state.commit_locked(&mut inner).unwrap();
            }
            let selection = selected_head(&state, &session);
            let concurrent = state.clone();
            let concurrent_session = session.clone();
            let did_replace = Arc::new(AtomicBool::new(false));
            let observed = did_replace.clone();
            let hook: Box<dyn FnOnce()> = Box::new(move || {
                observed.store(true, Ordering::SeqCst);
                if matches!(replacement, "human" | "ordinary") {
                    concurrent
                        .cancel_queued_prompt(&concurrent_session, "original-ordinary-head")
                        .unwrap();
                } else if replacement == "stop" {
                    // Idle has no Stop action. Exercise the supported Active
                    // session with a missing runtime instead; public Stop
                    // claims its missing-runtime owner without a provider send.
                    let before = {
                        let mut inner = concurrent.inner.lock().unwrap();
                        let index = inner.find_session_index(&concurrent_session).unwrap();
                        inner.sessions[index].session.status = SessionStatus::Active;
                        inner.sessions[index].runtime_stop_generation
                    };
                    concurrent
                        .request_stop_session(&concurrent_session)
                        .unwrap();
                    // The route returns Stopping while its worker completes.
                    // Test the stale choice after completion, not merely the
                    // transient runtime hold; use the shared deadlock guard.
                    let guard = phase_sync::PollGuard::new();
                    loop {
                        let inner = concurrent.inner.lock().unwrap();
                        let record =
                            &inner.sessions[inner.find_session_index(&concurrent_session).unwrap()];
                        if !record.runtime_stop_in_progress {
                            assert_eq!(record.session.status, SessionStatus::Idle);
                            break;
                        }
                        drop(inner);
                        guard.wait("concurrent Stop lifecycle completes before selected Resume");
                    }
                    let inner = concurrent.inner.lock().unwrap();
                    assert!(
                        inner.sessions[inner.find_session_index(&concurrent_session).unwrap()]
                            .runtime_stop_generation
                            > before
                    );
                } else {
                    let mut inner = concurrent.inner.lock().unwrap();
                    let index = inner.find_session_index(&concurrent_session).unwrap();
                    inner.sessions[index].queued_prompts[0].pending_prompt.text =
                        "edited payload".into();
                    sync_pending_prompts(&mut inner.sessions[index]);
                    inner.stamp_session_at_index(index);
                    concurrent.commit_locked(&mut inner).unwrap();
                }
            });
            match phase {
                "before_resume" => TEST_CURRENT_HUMAN_SELECTED_BEFORE_RESUME
                    .with(|slot| *slot.borrow_mut() = Some(hook)),
                "before_reset" => {
                    TEST_HUMAN_BEFORE_RUNTIME_RESET.with(|slot| *slot.borrow_mut() = Some(hook))
                }
                _ => TEST_HUMAN_BEFORE_PROMOTION.with(|slot| *slot.borrow_mut() = Some(hook)),
            }
            // Same synchronous boundary as the blocking HTTP Resume handler;
            // the interleaving and its assertion run on that execution thread.
            let error = state
                .resume_selected_human_prompt(&session, selection)
                .err()
                .expect("stale selection must refuse");
            assert!(
                did_replace.load(Ordering::SeqCst),
                "{phase}/{replacement}: hook ran"
            );
            assert_eq!(
                error.status,
                StatusCode::CONFLICT,
                "{phase}/{replacement}: {}",
                error.message
            );
            assert!(
                receiver.try_recv().is_err(),
                "{phase}/{replacement}: no replacement send"
            );
            {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                assert!(record.orchestrator_auto_dispatch_blocked);
                assert!(record.human_prompt_turn.is_none());
                assert_eq!(record.queued_prompts.len(), 1);
                assert!(record.human_queue_choices.is_empty());
            }
            // A new, exact choice belongs to the replacement rather than the
            // stale click. It remains independently resumable once, by id.
            let fresh = selected_head(&state, &session);
            let expected = fresh.prompt.text.clone();
            state
                .resume_selected_human_prompt(&session, fresh)
                .unwrap_or_else(|error| {
                    panic!(
                        "{phase}/{replacement}: fresh exact Resume failed: {}",
                        error.message
                    )
                });
            assert!(
                matches!(phase_sync::receive(&receiver, "new exact selected owner"),
                CodexRuntimeCommand::Prompt { command, .. } if command.prompt.contains(&expected))
            );
            assert!(receiver.try_recv().is_err());
        }
    }
}

#[tokio::test]
async fn human_prompt_failed_initial_sql_commit_restores_queue_before_http_retry() {
    let (state, session, receiver) = ordinary_selection_fixture();
    let before = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        serde_json::to_value(projected_pending_prompts(record)).unwrap()
    };
    let failed_root = TestTempRoot::create("human-initial-sql-failure");
    state.shutdown_persist_blocking();
    let mut failing = state.clone();
    // A directory cannot be opened as the SQLite file. This exercises the
    // actual disconnected-writer synchronous commit, not an injected return.
    failing.persistence_path = Arc::new(failed_root.path().to_path_buf());
    let request = || {
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session}/messages"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"text":"one retry after persistence recovers", "origin":"human"})
                    .to_string(),
            ))
            .unwrap()
    };
    let response = app_router(failing).oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains("failed to persist human prompt"));
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(
            serde_json::to_value(projected_pending_prompts(record)).unwrap(),
            before
        );
        assert_eq!(record.session.status, SessionStatus::Idle);
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(record.human_prompt_turn.is_none());
        assert!(
            record
                .queued_prompts
                .iter()
                .all(|head| head.source != QueuedPromptSource::Human)
        );
    }
    assert!(receiver.try_recv().is_err());
    // The failed invocation left no executable copy. A single UI retry after
    // recovery preserves FIFO and executes the submitted Human payload once.
    let response = app_router(state.clone()).oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    for expected in [
        "ordinary original payload",
        "one retry after persistence recovers",
    ] {
        let command = match phase_sync::receive(&receiver, expected) {
            CodexRuntimeCommand::Prompt { command, .. } => command,
            _ => panic!("expected original FIFO then Human retry"),
        };
        assert!(command.prompt.contains(expected));
        let token = {
            let inner = state.inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&session).unwrap()]
                .runtime
                .runtime_token()
                .unwrap()
        };
        state
            .finish_turn_ok_if_runtime_and_generation_match(
                &session,
                &token,
                command.active_turn_generation,
            )
            .unwrap();
    }
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    assert!(
        inner.sessions[inner.find_session_index(&session).unwrap()]
            .queued_prompts
            .is_empty()
    );
}

#[tokio::test]
async fn human_prompt_resume_is_idempotent_for_unpaused_busy_session() {
    for paused in [false, true] {
        let (state, session, receiver) = ordinary_selection_fixture();
        let generation = {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            record.queued_prompts[0].source = QueuedPromptSource::Human;
            record.session.status = SessionStatus::Active;
            record.set_auto_dispatch_blocked(paused);
            record.active_turn_generation
        };
        let response = app_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{session}/queue/resume"))
                    .header("content-type", "application/json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if paused {
                StatusCode::CONFLICT
            } else {
                StatusCode::OK
            }
        );
        assert!(receiver.try_recv().is_err());
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.session.status, SessionStatus::Active);
        assert_eq!(record.active_turn_generation, generation);
        assert_eq!(record.orchestrator_auto_dispatch_blocked, paused);
        assert_eq!(record.queued_prompts.len(), 1);
    }
}

#[tokio::test]
async fn human_prompt_accepted_intake_survives_final_provider_rejection() {
    for older_fifo in [true, false] {
        let (state, session, closed_receiver) = ordinary_selection_fixture();
        drop(closed_receiver);
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            record.set_auto_dispatch_blocked(false);
            if !older_fifo {
                record.queued_prompts.clear();
            }
        }
        let response = app_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{session}/messages"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"text":"accepted exactly once", "origin":"human"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let accepted_id = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert_ne!(record.session.status, SessionStatus::Active);
            assert!(
                record.session.preview.contains("failed to queue prompt"),
                "{}",
                record.session.preview
            );
            let humans: Vec<_> = record
                .queued_prompts
                .iter()
                .filter(|head| head.source == QueuedPromptSource::Human)
                .collect();
            assert_eq!(humans.len(), 1);
            assert_eq!(humans[0].pending_prompt.text, "accepted exactly once");
            humans[0].pending_prompt.id.clone()
        };
        if older_fifo {
            let (runtime, receiver, _process) =
                test_shared_codex_runtime("recovered-human-delivery");
            install_single_wire_codex_fixture(&state, Some(runtime));
            state.resume_session_queue(&session).unwrap();
            let command = match phase_sync::receive(&receiver, "retry accepted original Human") {
                CodexRuntimeCommand::Prompt { command, .. } => command,
                _ => panic!("expected original Human prompt"),
            };
            assert!(command.prompt.contains("accepted exactly once"));
            let token = {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                assert_eq!(
                    record.human_prompt_turn.as_ref().unwrap().prompt_id,
                    accepted_id
                );
                record.runtime.runtime_token().unwrap()
            };
            state
                .finish_turn_ok_if_runtime_and_generation_match(
                    &session,
                    &token,
                    command.active_turn_generation,
                )
                .unwrap();
            assert!(
                receiver.try_recv().is_err(),
                "no duplicate after completion"
            );
            let inner = state.inner.lock().unwrap();
            assert!(
                inner.sessions[inner.find_session_index(&session).unwrap()]
                    .queued_prompts
                    .is_empty()
            );
        }
    }
}

#[tokio::test]
async fn human_prompt_ui_choice_preserves_ordinary_known_unpromoted_resume_without_conversion() {
    let (state, session, receiver) = ordinary_selection_fixture();
    let selected = selected_head(&state, &session);
    let response = app_router(state.clone()).oneshot(Request::builder().method("POST")
        .uri(format!("/api/sessions/{session}/queue/resume")).header("content-type", "application/json")
        .body(Body::from(json!({"origin":"human", "humanChoice": {
            "prompt":selected.prompt, "sessionMutationStamp":selected.session_mutation_stamp
        }}).to_string())).unwrap()).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert!(
        matches!(receiver.try_recv().unwrap(), CodexRuntimeCommand::Prompt { command, .. }
        if command.prompt.contains("ordinary original payload"))
    );
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(!record.orchestrator_auto_dispatch_blocked);
    assert!(record.human_prompt_turn.is_none());
    assert!(record.human_queue_choices.is_empty());
    assert!(record.prompt_deliveries.is_empty());
    assert!(record.session.messages.iter().any(|message|
        matches!(message, Message::Text { id, text, .. } if id=="original-ordinary-head" && text=="ordinary original payload")));
}

#[test]
fn human_prompt_ordinary_fallback_refuses_unknown_disposition_handoff_or_replaced_owner() {
    for case in ["missing_disposition", "possible_handoff", "replaced"] {
        let (state, session, receiver) = ordinary_selection_fixture();
        if case != "replaced" {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let head = &mut inner.sessions[index].queued_prompts[0];
            if case == "missing_disposition" {
                head.promotion_disposition_known = false;
            } else {
                head.engram_ordinary_handoff = true;
            }
        }
        let selected = selected_head(&state, &session);
        if case == "replaced" {
            let hook_state = state.clone();
            let hook_session = session.clone();
            TEST_ORDINARY_SELECTED_BEFORE_RESUME.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || {
                    let mut inner = hook_state.inner.lock().unwrap();
                    let index = inner.find_session_index(&hook_session).unwrap();
                    let mut replacement = inner.sessions[index].queued_prompts[0].clone();
                    replacement.pending_prompt.id = "replacement-head".into();
                    replacement.pending_prompt.text = "successor content".into();
                    inner.sessions[index].queued_prompts.push_front(replacement);
                    inner.stamp_session_at_index(index);
                }))
            });
        }
        assert!(
            state
                .resume_selected_human_prompt(&session, selected)
                .is_err()
        );
        assert!(receiver.try_recv().is_err());
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(
            record
                .queued_prompts
                .iter()
                .any(|head| head.pending_prompt.id == "original-ordinary-head"
                    && head.pending_prompt.text == "ordinary original payload"
                    && head.source == QueuedPromptSource::User)
        );
        assert!(record.prompt_deliveries.is_empty());
        if case == "replaced" {
            assert!(record.human_queue_choices.is_empty());
            assert_eq!(
                record.queued_prompts[0].pending_prompt.id,
                "replacement-head"
            );
        }
    }
}

#[test]
fn human_prompt_queued_head_never_exempts_a_peer_or_default_send_from_control() {
    for peer in [false, true] {
        let (state, _project, session, _root, receiver) = fixture();
        let transport = ScriptedEngramControlTransport::new([]);
        install_control_only_transport(&state, transport.clone());
        enqueue_human_for_resume(&state, &session, false);
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].session.status = SessionStatus::Idle;
            inner.sessions[index].set_auto_dispatch_blocked(true);
            assert!(AppState::engram_session_requires_dispatch_card_locked(
                &inner, &session
            ));
        }
        let mut request = prompt();
        request.text = "automated successor must remain gated".into();
        if peer {
            request.source_session_id = Some(test_session_id(&state, Agent::Claude));
        }
        assert!(matches!(
            state.dispatch_turn(&session, request).unwrap(),
            DispatchTurnResult::Queued
        ));
        assert!(receiver.try_recv().is_err());
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.human_prompt_turn.is_none());
        assert_eq!(record.queued_prompts[0].source, QueuedPromptSource::Human);
        assert_eq!(record.queued_prompts[1].source, QueuedPromptSource::User);
        assert!(transport.requests().is_empty());
    }
}

#[test]
fn human_prompt_paused_head_cannot_be_promoted_by_off_or_unconfigured_ordinary_send() {
    for configured in [false, true] {
        for peer in [false, true] {
            let (state, project, session, _root, receiver) = fixture();
            {
                let mut inner = state.inner.lock().unwrap();
                let project = inner.projects.iter_mut().find(|p| p.id == project).unwrap();
                if configured {
                    project.engram.as_mut().unwrap().turn_gated_control = false;
                } else {
                    project.engram = None;
                }
                let index = inner.find_session_index(&session).unwrap();
                inner.sessions[index].engram = EngramSessionState::default();
                inner.sessions[index].session.status = SessionStatus::Idle;
            }
            enqueue_human_for_resume(&state, &session, false);
            {
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&session).unwrap();
                inner.sessions[index].set_auto_dispatch_blocked(true);
            }
            let mut request = prompt();
            request.text = "unrelated ordinary send".into();
            if peer {
                request.source_session_id = Some(test_session_id(&state, Agent::Claude));
            }
            assert!(matches!(
                state.dispatch_turn(&session, request).unwrap(),
                DispatchTurnResult::Queued
            ));
            assert!(receiver.try_recv().is_err());
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(record.orchestrator_auto_dispatch_blocked);
            assert_eq!(record.session.status, SessionStatus::Idle);
            assert!(record.human_prompt_turn.is_none());
            assert_eq!(record.queued_prompts[0].source, QueuedPromptSource::Human);
            assert_eq!(record.queued_prompts[1].source, QueuedPromptSource::User);
        }
    }
}

#[tokio::test]
async fn human_prompt_ui_send_activates_a_dormant_ordinary_fifo_without_relabelling() {
    for paused in [false, true] {
        let (state, session, receiver) = ordinary_selection_fixture();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            let saved = PersistedSessionRecord::from_record(record);
            *record = saved.into_record().unwrap();
            record.set_auto_dispatch_blocked(paused);
            assert!(ordinary_known_unpromoted_head(
                record,
                &record.queued_prompts[0]
            ));
        }
        let response = app_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{session}/messages"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"text":"new explicit Human behind FIFO", "origin":"human"})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let first = match receiver.try_recv().unwrap() {
            CodexRuntimeCommand::Prompt { command, .. } => command,
            _ => panic!("ordinary FIFO activation expected"),
        };
        assert!(first.prompt.contains("ordinary original payload"));
        let runtime = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(record.human_prompt_turn.is_none());
            assert!(!record.orchestrator_auto_dispatch_blocked);
            assert_eq!(record.queued_prompts.len(), 1);
            assert_eq!(record.queued_prompts[0].source, QueuedPromptSource::Human);
            record.runtime.runtime_token().unwrap()
        };
        state
            .finish_turn_ok_if_runtime_and_generation_match(
                &session,
                &runtime,
                first.active_turn_generation,
            )
            .unwrap();
        assert!(
            matches!(phase_sync::receive(&receiver, "Human successor after normal FIFO completion"),
            CodexRuntimeCommand::Prompt { command, .. } if command.prompt.contains("new explicit Human behind FIFO"))
        );
        assert!(receiver.try_recv().is_err());
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.queued_prompts.is_empty());
        assert!(human_prompt_turn_is_current(record));
    }
}

#[test]
fn human_prompt_send_preserves_a_refused_relay_carrier_and_starts_its_own_envelope() {
    let (state, session, receiver) = ordinary_selection_fixture();
    let original = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        record.set_auto_dispatch_blocked(false);
        let head = &mut record.queued_prompts[0];
        head.source = QueuedPromptSource::Mailbox;
        head.pending_prompt.source = Some(MessageSource {
            session_id: None,
            name: "retained unavailable relay".into(),
            kind: MessageSourceKind::Mailbox,
            mailbox: Some(MailboxMessageSource {
                mailbox_id: "relay-mailbox-invalid-locator".into(),
                message_id: "retained-relay-message".into(),
                sequence: 1,
                unread_count: 1,
            }),
        });
        let original = serde_json::to_value(head).unwrap();
        state.commit_locked(&mut inner).unwrap();
        original
    };
    assert!(
        state
            .revalidate_queued_mailbox_wakeups_before_dispatch(&session)
            .is_err()
    );
    let mut request = prompt();
    request.text = "new Human beside refused relay".into();
    let dispatch = match state.dispatch_human_turn(&session, request).unwrap() {
        DispatchTurnResult::Dispatched(d) | DispatchTurnResult::DispatchedAfterQueue(d) => d,
        _ => panic!("explicit Human activation may pass the refused carrier"),
    };
    assert!(dispatch.human_prompt_id().is_some());
    assert!(matches!(
        deliver_turn_dispatch(&state, dispatch),
        TurnDispatchDeliveryOutcome::Delivered
    ));
    assert!(
        matches!(receiver.try_recv().unwrap(), CodexRuntimeCommand::Prompt { command, .. }
        if command.prompt.contains("new Human beside refused relay"))
    );
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(
        serde_json::to_value(&record.queued_prompts[0]).unwrap(),
        original
    );
}

#[test]
fn human_prompt_concurrent_promotion_before_reset_returns_accepted_without_duplicate() {
    let (state, session, receiver) = ordinary_selection_fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].queued_prompts.clear();
    }
    let winner = state.clone();
    let winner_session = session.clone();
    let competed = Arc::new(AtomicBool::new(false));
    let did_compete = competed.clone();
    TEST_HUMAN_BEFORE_RUNTIME_RESET.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            did_compete.store(true, Ordering::SeqCst);
            let HumanPromptDrain::Started(started) = winner
                .start_human_queued_turn(&winner_session, true)
                .unwrap()
            else {
                panic!("competing owner must promote the same accepted Human");
            };
            assert!(matches!(
                deliver_turn_dispatch(&winner, started.dispatch),
                TurnDispatchDeliveryOutcome::Delivered
            ));
        }))
    });
    // The HTTP handler runs this synchronous boundary on a blocking thread.
    // Keep the thread-local interleaving on that same execution thread here;
    // route status and origin parsing are covered by the HTTP fixtures.
    let mut request = prompt();
    request.text = "accepted by concurrent owner".into();
    dispatch_turn_and_snapshot_with_origin(
        &state,
        &session,
        request,
        Some(ExplicitPromptOrigin::Human),
    )
    .unwrap();
    assert!(
        competed.load(Ordering::SeqCst),
        "the competing promotion must actually run"
    );
    assert!(
        matches!(receiver.try_recv().unwrap(), CodexRuntimeCommand::Prompt { command, .. }
        if command.prompt.contains("accepted by concurrent owner"))
    );
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(record.queued_prompts.is_empty());
    assert!(human_prompt_turn_is_current(record));
}

#[test]
fn human_prompt_remote_choice_translates_only_the_exact_cached_projection_version() {
    for remote_changed in [false, true] {
        let (upstream, upstream_session, receiver) = ordinary_selection_fixture();
        let (remote_wire, remote_stamp) = {
            let inner = upstream.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&upstream_session).unwrap()];
            (
                AppState::wire_session_from_record(&upstream.server_instance_id, record),
                record.mutation_stamp,
            )
        };
        let local = test_app_state();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let remote = RemoteConfig {
            id: "human-choice-peer".into(),
            name: "Human choice peer".into(),
            transport: RemoteTransport::Ssh,
            enabled: true,
            host: Some("fixture.invalid".into()),
            port: Some(22),
            user: Some("fixture".into()),
        };
        let local_session = {
            let mut inner = local.inner.lock().unwrap();
            inner.preferences.remotes.push(remote.clone());
            let publication = local
                .remote_registry
                .publish_configs(&inner.preferences.remotes);
            let session =
                upsert_remote_proxy_session_record(&mut inner, &remote.id, &remote_wire, None);
            let index = inner.find_session_index(&session).unwrap();
            for _ in 0..20 {
                inner.stamp_session_at_index(index);
            }
            local.commit_locked(&mut inner).unwrap();
            drop(inner);
            local.remote_registry.finish_config_publication(publication);
            session
        };
        local.remote_registry.connections.lock().unwrap().insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: remote,
                authority_generation: 0,
                retired: AtomicBool::new(false),
                state_continuity_generation: AtomicU64::new(1),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(false),
                event_bridge_shutdown: AtomicBool::new(false),
            }),
        );
        let selected = {
            let inner = local.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&local_session).unwrap()];
            let wire = AppState::wire_session_from_record(&local.server_instance_id, record);
            assert_ne!(wire.session_mutation_stamp, Some(remote_stamp));
            HumanPromptSelection {
                prompt: wire.pending_prompts[0].clone(),
                session_mutation_stamp: wire.session_mutation_stamp.unwrap(),
            }
        };
        let mut stale_local = selected.clone();
        stale_local.session_mutation_stamp += 1;
        assert_eq!(
            local
                .resume_selected_human_prompt(&local_session, stale_local)
                .err()
                .unwrap()
                .status,
            StatusCode::CONFLICT,
            "reject stale local view before forwarding"
        );
        if remote_changed {
            let mut inner = upstream.inner.lock().unwrap();
            let index = inner.find_session_index(&upstream_session).unwrap();
            inner.stamp_session_at_index(index);
        }
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let captured = requests.clone();
        let serving = upstream.clone();
        let server = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let mut posts = 0;
            while posts < if remote_changed { 1 } else { 2 } {
                let mut stream = accept_test_connection(&listener, "Human proxy fixture");
                let request = read_test_http_request(&mut stream);
                if request.request_line.starts_with("GET /api/health ") {
                    write_test_http_response(
                        &mut stream,
                        StatusCode::OK,
                        "application/json",
                        &json!({"ok":true,"serverInstanceId":serving.server_instance_id})
                            .to_string(),
                    );
                    continue;
                }
                assert!(request.request_line.starts_with("POST /api/sessions/"));
                let uri = request
                    .request_line
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .to_owned();
                captured
                    .lock()
                    .unwrap()
                    .push(serde_json::from_str(&request.body).unwrap());
                let response = runtime
                    .block_on(
                        app_router(serving.clone()).oneshot(
                            Request::builder()
                                .method("POST")
                                .uri(uri)
                                .header("content-type", "application/json")
                                .body(Body::from(request.body))
                                .unwrap(),
                        ),
                    )
                    .unwrap();
                let status = response.status();
                let bytes = runtime
                    .block_on(axum::body::to_bytes(response.into_body(), usize::MAX))
                    .unwrap();
                write_test_http_response(
                    &mut stream,
                    status,
                    "application/json",
                    std::str::from_utf8(&bytes).unwrap(),
                );
                posts += 1;
            }
        });
        let result = local.resume_selected_human_prompt(&local_session, selected);
        if remote_changed {
            assert!(
                result.is_err(),
                "remote owner still refuses a stale upstream version"
            );
            assert!(receiver.try_recv().is_err());
        } else {
            result.unwrap();
            assert!(
                matches!(receiver.try_recv().unwrap(), CodexRuntimeCommand::Prompt { command, .. }
                if command.prompt.contains("ordinary original payload"))
            );
            local.dispatch_human_turn(&local_session, prompt()).unwrap();
            let inner = upstream.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&upstream_session).unwrap()];
            assert!(
                record.human_prompt_turn.is_none(),
                "ordinary Resume gains no Human exemption"
            );
            assert_eq!(record.queued_prompts.len(), 1);
            assert_eq!(record.queued_prompts[0].source, QueuedPromptSource::Human);
            assert!(receiver.try_recv().is_err());
        }
        server.join().unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(
            requests[0]["humanChoice"]["sessionMutationStamp"],
            remote_stamp
        );
        if !remote_changed {
            assert_eq!(requests[1]["origin"], "human");
            assert!(requests[1].get("sourceSessionId").is_none());
        }
    }
}

#[test]
fn human_prompt_live_unknown_handoff_never_enters_control_on_admission() {
    for entry in [
        "resume",
        "bound_resume",
        "drain",
        "prepare",
        "admission",
        "inner",
    ] {
        let paused = matches!(entry, "resume" | "bound_resume");
        let (state, _project, session, _root, receiver) = fixture();
        let transport = ScriptedEngramControlTransport::new([]);
        install_control_only_transport(&state, transport.clone());
        let dispatch = match state.dispatch_human_turn(&session, prompt()).unwrap() {
            DispatchTurnResult::Dispatched(d) | DispatchTurnResult::DispatchedAfterQueue(d) => d,
            DispatchTurnResult::Queued => panic!("idle Human must prepare under control ON"),
        };
        let original_id = dispatch.human_prompt_id().unwrap().to_owned();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].engram.project_reset_in_progress = true;
        }
        assert!(matches!(
            deliver_turn_dispatch(&state, dispatch),
            TurnDispatchDeliveryOutcome::Rejected(_)
        ));
        let (generation, grant, rebind, intent, obligations) = {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            record.engram.project_reset_in_progress = false;
            record.set_auto_dispatch_blocked(paused);
            let head = record.queued_prompts.front().unwrap();
            assert_eq!(head.source, QueuedPromptSource::Human);
            assert_eq!(head.pending_prompt.id, original_id);
            assert!(head.engram_ordinary_handoff);
            assert!(!head.engram_interrupted, "live failure, not reload's fence");
            assert_eq!(record.session.status, SessionStatus::Error);
            assert!(matches!(record.runtime, SessionRuntime::None));
            // Rejection retires the runtime, retaining lineage for the old
            // operation's independent accounting; it is not a new live send.
            let owner = record.human_prompt_turn.as_ref().unwrap();
            assert_eq!(owner.prompt_id, original_id);
            assert_eq!(owner.generation, record.active_turn_generation);
            assert!(AppState::engram_session_requires_dispatch_card_locked(
                &inner, &session
            ));
            let record = &inner.sessions[index];
            (
                record.engram.dispatch_generation,
                record.engram.active_grant_id.clone(),
                record.engram.rebind_required,
                receipted_prompt_intent(record.queued_prompts.front().unwrap()),
                serde_json::to_value(&inner.engram_session_obligations).unwrap(),
            )
        };
        assert!(transport.requests().is_empty());
        assert!(receiver.try_recv().is_err());
        match entry {
            "resume" => {
                let refusal = state
                    .resume_session_queue(&session)
                    .err()
                    .expect("unknown provider handoff must remain refused");
                assert_eq!(refusal.status, StatusCode::CONFLICT);
                assert!(
                    refusal
                        .message
                        .contains("may already have reached its provider")
                );
            }
            "bound_resume" => {
                let owner = {
                    let inner = state.inner.lock().unwrap();
                    EngramQueuedAdmissionOwner::capture(
                        &inner.sessions[inner.find_session_index(&session).unwrap()],
                    )
                    .unwrap()
                };
                assert!(
                    state
                        .dispatch_next_queued_turn_for_resume(&session, owner)
                        .unwrap()
                        .is_none()
                );
            }
            "drain" => assert!(
                state
                    .dispatch_next_queued_turn(&session, false)
                    .unwrap()
                    .is_none()
            ),
            "prepare" => assert!(
                state
                    .prepare_next_queued_turn_engram_off_lock(&session)
                    .is_none()
            ),
            "admission" => assert!(
                state
                    .with_queued_engram_admission(&session, || {
                        panic!("a Human head must not acquire an ordinary admission guard")
                    })
                    .unwrap()
                    .is_none()
            ),
            "inner" => assert!(
                state
                    .start_next_queued_turn_inner_off_lock(&session, false, false, None)
                    .unwrap()
                    .is_none()
            ),
            _ => unreachable!(),
        }
        assert!(
            transport.requests().is_empty(),
            "{entry}: Human is never evaluated"
        );
        assert!(
            receiver.try_recv().is_err(),
            "{entry}: unknown handoff is not replayed"
        );
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.engram.dispatch_generation, generation, "{entry}");
        assert_eq!(record.engram.active_grant_id, grant, "{entry}");
        assert_eq!(record.engram.rebind_required, rebind, "{entry}");
        assert!(record.engram.admission_in_progress.is_none());
        assert_eq!(record.session.status, SessionStatus::Error);
        assert!(matches!(record.runtime, SessionRuntime::None));
        let owner = record.human_prompt_turn.as_ref().unwrap();
        assert_eq!(owner.prompt_id, original_id);
        assert_eq!(owner.generation, record.active_turn_generation);
        assert_eq!(record.orchestrator_auto_dispatch_blocked, paused);
        assert_eq!(record.queued_prompts.len(), 1);
        let head = record.queued_prompts.front().unwrap();
        assert_eq!(head.pending_prompt.id, original_id);
        assert_eq!(receipted_prompt_intent(head), intent);
        assert!(head.engram_ordinary_handoff && !head.engram_interrupted);
        assert_eq!(
            serde_json::to_value(&inner.engram_session_obligations).unwrap(),
            obligations
        );
    }
}

#[test]
fn human_prompt_denied_final_handoff_releases_only_its_unsent_runtime() {
    let (state, _project, session, receiver, transport) = queue::held_fixture();
    let dispatch = match state.dispatch_human_turn(&session, prompt()).unwrap() {
        DispatchTurnResult::Dispatched(d) | DispatchTurnResult::DispatchedAfterQueue(d) => d,
        _ => panic!("initial human turn must prepare"),
    };
    let original =
        serde_json::to_value(&state.inner.lock().unwrap().engram_session_obligations).unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram.project_reset_in_progress = true;
    }
    assert!(matches!(
        deliver_turn_dispatch(&state, dispatch),
        TurnDispatchDeliveryOutcome::Rejected(_)
    ));
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.session.status, SessionStatus::Error);
    assert!(
        record
            .queued_prompts
            .iter()
            .any(|head| head.source == QueuedPromptSource::Human && head.engram_ordinary_handoff)
    );
    assert_eq!(
        serde_json::to_value(&inner.engram_session_obligations).unwrap(),
        original
    );
    assert_eq!(transport.requests().len(), 2);
}

#[cfg(windows)]
#[test]
fn human_prompt_one_legacy_resume_carries_exact_receipt_across_dedicated_runtime_reset() {
    use crate::tests::delegation_process_cleanup::ProductionWorkerTree;
    let (mut state, _project, session, receiver, transport) =
        human_choices::legacy_pending_fixture();
    let temp = TestTempRoot::create("legacy-human-dedicated-to-shared-reset");
    // A retained dedicated owner from the previous runtime configuration is
    // retired before the current shared-Codex destination can be promoted.
    state.agent_runtime_spawning_enabled = true;
    let old = ProductionWorkerTree::spawn(&state, &session, temp.path(), false);
    state.agent_runtime_spawning_enabled = false;
    let prior_stop = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        record.session.status = SessionStatus::Idle;
        record.runtime_reset_required = true;
        record.runtime_stop_generation
    };
    let (claimed_tx, claimed_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *old.tree.cleanup_gate.lock().unwrap() = Some(TestStopFenceGate {
        claimed_tx,
        release_rx,
    });
    let writer = human_choices::ChoiceWriter::attach(&mut state);
    state
        .resume_selected_human_prompt(&session, selected_head(&state, &session))
        .unwrap();
    let ack = writer.write_ahead();
    assert!(receiver.try_recv().is_err());
    ack.finish(Ok(()));
    phase_sync::receive(&claimed_rx, "the selected receipt owns its dedicated reset");
    assert!(state.inner.inner.try_lock().is_ok());
    release_tx.send(()).unwrap();
    assert!(
        matches!(phase_sync::receive(&receiver,"one Resume delivers after its own successful reset"),
        CodexRuntimeCommand::Prompt{command,..} if command.prompt.contains("Exactly this legacy content"))
    );
    old.await_exit();
    old.assert_peers_exited();
    assert!(old.tree.is_confirmed());
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(record.runtime_stop_generation > prior_stop);
    assert_eq!(
        record.human_queue_choices["legacy-proven-pending"].stop_generation,
        record.runtime_stop_generation
    );
    assert_eq!(
        record.prompt_deliveries["legacy-proven-pending"]
            .entry
            .as_ref()
            .unwrap()
            .source,
        QueuedPromptSource::User
    );
    assert!(record.queued_prompts.is_empty());
    assert!(human_prompt_turn_is_current(record));
    assert_eq!(transport.requests().len(), 2);
}

#[test]
fn human_prompt_refused_choice_does_not_create_a_new_pause() {
    let (state, _project, session, receiver, _transport) = human_choices::legacy_pending_fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].set_auto_dispatch_blocked(false);
    }
    let selected = selected_head(&state, &session);
    let hook_state = state.clone();
    let hook_session = session.clone();
    TEST_CONTAINMENT_BEFORE_ACK_RETURN.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            let mut inner = hook_state.inner.lock().unwrap();
            let index = inner.find_session_index(&hook_session).unwrap();
            let mut replacement = inner.sessions[index].queued_prompts[0].clone();
            replacement.pending_prompt.id = "new-head-after-choice".into();
            inner.sessions[index].queued_prompts.push_front(replacement);
            inner.stamp_session_at_index(index);
        }))
    });
    assert!(
        state
            .resume_selected_human_prompt(&session, selected)
            .is_err()
    );
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(!record.orchestrator_auto_dispatch_blocked);
    assert_eq!(
        record.prompt_deliveries["legacy-proven-pending"].disposition,
        PromptDeliveryDisposition::Pending
    );
    assert_eq!(record.queued_prompts.len(), 2);
}

#[test]
fn human_prompt_legacy_promotion_failure_returns_exact_receipt_to_pending_and_retries_once() {
    for capture in [true, false] {
        let (mut state, _project, session, receiver, transport) =
            human_choices::legacy_pending_fixture();
        let writer = human_choices::ChoiceWriter::attach(&mut state);
        state
            .resume_selected_human_prompt(&session, selected_head(&state, &session))
            .unwrap();
        let wal = writer.write_ahead();
        let (prompt_id, nonce) = match &wal.target {
            PersistFenceTarget::PromptDeliveryWriteAhead {
                prompt_id,
                write_ahead,
                ..
            } => (prompt_id.clone(), write_ahead.clone()),
            _ => panic!("actual write-ahead fence expected"),
        };
        let duties =
            serde_json::to_value(&state.inner.lock().unwrap().engram_session_obligations).unwrap();
        // The writer already proved this exact WAL in real SQLite. Run its
        // production settlement on a joined thread so capture injection is
        // scoped to this continuation, without a scheduler timing assumption.
        let callback_state = state.clone();
        let callback_session = session.clone();
        let callback_id = prompt_id.clone();
        let callback_nonce = nonce.clone();
        let wal = (*wal).on_resolved(move |result| {
            if capture {
                TEST_HUMAN_PROMOTION_CAPTURE_FAILURE.with(|flag| flag.set(true));
            }
            callback_state.settle_prompt_delivery_write_ahead(
                &callback_session,
                &callback_id,
                &callback_nonce,
                result,
            );
        });
        if !capture {
            writer.hold_next_promotion();
        }
        let worker = std::thread::spawn(move || wal.finish(Ok(())));
        let rejected = if capture {
            None
        } else {
            let promotion = writer.write_ahead();
            assert!(matches!(
                &promotion.target,
                PersistFenceTarget::EngramProjectContainment(_)
            ));
            promotion.finish(Err(PersistFenceError::WriteFailed(
                "rejected legacy promotion".into(),
            )));
            Some(promotion)
        };
        worker.join().unwrap();
        assert!(
            receiver.try_recv().is_err(),
            "failed promotion never reaches provider"
        );
        {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            let receipt = &record.prompt_deliveries[&prompt_id];
            assert_eq!(receipt.disposition, PromptDeliveryDisposition::Pending);
            assert!(receipt.turn_generation.is_none());
            assert_ne!(receipt.write_ahead, nonce);
            assert_eq!(record.session.status, SessionStatus::Error);
            assert_eq!(record.queued_prompts[0].pending_prompt.id, prompt_id);
            assert_eq!(
                record.queued_prompts[0].pending_prompt.text,
                "Exactly this legacy content"
            );
            assert_eq!(record.queued_prompts[0].source, QueuedPromptSource::User);
            assert_eq!(
                serde_json::to_value(&inner.engram_session_obligations).unwrap(),
                duties
            );
        }
        state
            .resume_selected_human_prompt(&session, selected_head(&state, &session))
            .unwrap();
        writer.write_ahead().finish(Ok(()));
        let command = match phase_sync::receive(&receiver, "same legacy prompt retried once") {
            CodexRuntimeCommand::Prompt { command, .. } => command,
            _ => panic!("prompt expected"),
        };
        assert_eq!(
            command.delivery_receipt.as_ref().unwrap().prompt_id,
            prompt_id
        );
        assert!(command.prompt.contains("Exactly this legacy content"));
        if let Some(late) = rejected {
            late.finish(Ok(()));
        }
        state.settle_prompt_delivery_write_ahead(&session, &prompt_id, &nonce, Ok(()));
        assert!(
            receiver.try_recv().is_err(),
            "late old outcomes cannot replay the prompt"
        );
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(
            record.active_turn_generation,
            command.active_turn_generation
        );
        assert!(record.queued_prompts.is_empty());
        assert_eq!(transport.requests().len(), 2);
    }
}

#[test]
fn human_prompt_busy_receipted_choice_reports_conflict_without_a_continuation() {
    for status in [
        SessionStatus::Active,
        SessionStatus::Approval,
        SessionStatus::Stopping,
    ] {
        let (state, _project, session, receiver, _) = human_choices::legacy_pending_fixture();
        let changed_state = state.clone();
        let changed_session = session.clone();
        TEST_CONTAINMENT_BEFORE_ACK_RETURN.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                let mut inner = changed_state.inner.lock().unwrap();
                let index = inner.find_session_index(&changed_session).unwrap();
                inner.sessions[index].session.status = status;
            }))
        });
        assert_eq!(
            state
                .resume_selected_human_prompt(&session, selected_head(&state, &session))
                .err()
                .unwrap()
                .status,
            StatusCode::CONFLICT
        );
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        let receipt = &record.prompt_deliveries["legacy-proven-pending"];
        assert_eq!(receipt.disposition, PromptDeliveryDisposition::Pending);
        assert!(receipt.resume.is_none());
        assert!(receiver.try_recv().is_err());
    }
}

#[tokio::test]
async fn human_prompt_ui_resume_cancels_capacity_owner_before_selected_or_human_dispatch() {
    for human in [false, true] {
        let (state, session, _unused_receiver) = ordinary_selection_fixture();
        let (shared, receiver, process) = test_shared_codex_runtime("selected-capacity-resume");
        let (runtime, generation) = {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                runtime_id: shared.runtime_id.clone(),
                input_tx: shared.input_tx.clone(),
                process,
                shared_session: Some(SharedCodexSessionHandle {
                    runtime: shared.clone(),
                    session_id: session.clone(),
                }),
            });
            record.set_auto_dispatch_blocked(false);
            if human {
                record.queued_prompts[0].source = QueuedPromptSource::Human;
            }
            (
                record.runtime.runtime_token().unwrap(),
                record.active_turn_generation,
            )
        };
        let nonce = state
            .prepare_provider_capacity_failure(
                &session,
                &runtime,
                Some(generation),
                true,
                &json!({"codexErrorInfo":"serverOverloaded"}),
                false,
            )
            .unwrap();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].session.status = SessionStatus::Error;
        }
        state
            .finish_provider_capacity_failure(&session, &nonce)
            .unwrap();
        {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(record.provider_capacity_retry.is_some());
            assert!(record.session.provider_capacity_waiting);
        }
        let selected = selected_head(&state, &session);
        let response = app_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{session}/queue/resume"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&ResumeQueueRequest {
                            origin: Some(ExplicitPromptOrigin::Human),
                            human_choice: Some(selected),
                        })
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(matches!(
            receiver.try_recv().unwrap(),
            CodexRuntimeCommand::Prompt { .. }
        ));
        assert!(receiver.try_recv().is_err());
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.provider_capacity_retry.is_none());
        assert!(!record.session.provider_capacity_waiting);
    }
}

#[tokio::test]
async fn human_prompt_resume_route_accepts_empty_json_body() {
    let state = test_app_state();
    let session = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].set_auto_dispatch_blocked(true);
    }
    let response = app_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{session}/queue/resume"))
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let inner = state.inner.lock().unwrap();
    assert!(
        !inner.sessions[inner.find_session_index(&session).unwrap()]
            .orchestrator_auto_dispatch_blocked
    );
}

#[tokio::test]
async fn human_prompt_ui_legacy_choice_keeps_unknown_head_but_delivers_distinct_successor() {
    let (state, project, session, receiver, transport) = queue::held_fixture();
    human_choices::enable_gated_legacy_selection_fixture(&state, &project, &session);
    enqueue_human_for_resume(&state, &session, false);
    let (selection, old, duties) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        (
            json!({"origin":"human", "humanChoice": {
                "prompt": projected_pending_prompts(record)[0], "sessionMutationStamp":record.mutation_stamp
            }}),
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            serde_json::to_value(&inner.engram_session_obligations).unwrap(),
        )
    };
    let response = app_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{session}/queue/resume"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&selection).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(
        serde_json::to_value(&record.queued_prompts[0]).unwrap(),
        old
    );
    assert!(record.prompt_deliveries.is_empty());
    assert!(
        record
            .human_queue_choices
            .contains_key("original-held-head")
    );
    assert_eq!(
        serde_json::to_value(&inner.engram_session_obligations).unwrap(),
        duties
    );
    assert_eq!(transport.requests().len(), 2);
}

#[test]
fn human_prompt_failed_promotion_capture_releases_only_unsent_turn_and_retains_debt() {
    let (state, _project, session, receiver, transport) = queue::held_fixture();
    let duties =
        serde_json::to_value(&state.inner.lock().unwrap().engram_session_obligations).unwrap();
    TEST_HUMAN_PROMOTION_CAPTURE_FAILURE.with(|flag| flag.set(true));
    assert!(
        matches!(
            state.dispatch_human_turn(&session, prompt()).unwrap(),
            DispatchTurnResult::Queued
        ),
        "the accepted envelope is retained even when its promotion image fails"
    );
    assert!(receiver.try_recv().is_err());
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.session.status, SessionStatus::Error);
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(record.session.preview.contains("capture failure"));
        assert!(
            record
                .session
                .preview
                .contains("delivery confirmation requires recovery")
        );
        assert_eq!(record.queued_prompts.len(), 2);
        assert!(record.queued_prompts[1].engram_ordinary_handoff);
        assert_eq!(
            serde_json::to_value(&inner.engram_session_obligations).unwrap(),
            duties
        );
    }
    let mut fresh = prompt();
    fresh.text = "Independent successor after failed image capture".into();
    let dispatch = match state.dispatch_human_turn(&session, fresh).unwrap() {
        DispatchTurnResult::Dispatched(d) | DispatchTurnResult::DispatchedAfterQueue(d) => d,
        DispatchTurnResult::Queued => {
            panic!("failed unsent predecessor cannot strand a distinct successor")
        }
    };
    assert!(matches!(
        deliver_turn_dispatch(&state, dispatch),
        TurnDispatchDeliveryOutcome::Delivered
    ));
    assert!(
        matches!(receiver.try_recv().unwrap(), CodexRuntimeCommand::Prompt { command, .. }
        if command.prompt.contains("Independent successor after failed image capture"))
    );
    assert!(receiver.try_recv().is_err());
    assert_eq!(transport.requests().len(), 2);
}

#[tokio::test]
async fn human_prompt_ui_followup_keeps_origin_for_completed_and_failed_children() {
    for status in [DelegationStatus::Completed, DelegationStatus::Failed] {
        let (state, project, parent, root, receiver) = fixture();
        let (delegation, child) = install_required_review_delegation(&state, &parent);
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_delegation_index(&delegation).unwrap();
            inner.delegations[index].mode = DelegationMode::Explorer;
            inner.delegations[index].status = status;
            inner.delegations[index].completed_at = Some(stamp_now());
            let index = inner.find_session_index(&child).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            record.session.project_id = Some(project);
            record.session.workdir = root.to_string_lossy().into_owned();
            record.session.status = SessionStatus::Idle;
            record.engram.routing_token = Some("old-child-token".into());
            record.engram.active_grant_id = Some("old-child-grant".into());
            record.engram.begins_recorded = true;
            state.commit_locked(&mut inner).unwrap();
        }
        let transport = ScriptedEngramControlTransport::new([]);
        install_control_only_transport(&state, transport.clone());
        let response = app_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{child}/messages"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"origin":"human","text":"Explicit child follow-up"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let code = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            code,
            StatusCode::ACCEPTED,
            "{}",
            String::from_utf8_lossy(&body)
        );
        assert!(
            matches!(receiver.try_recv().unwrap(), CodexRuntimeCommand::Prompt { command, .. }
            if command.prompt.contains("Explicit child follow-up"))
        );
        assert!(receiver.try_recv().is_err());
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&child).unwrap()];
        assert!(human_prompt_turn_is_current(record));
        assert_eq!(
            record.engram.active_grant_id.as_deref(),
            Some("old-child-grant")
        );
        assert_eq!(
            inner.delegations[inner.find_delegation_index(&delegation).unwrap()].status,
            DelegationStatus::Running
        );
        assert!(transport.requests().is_empty());
    }
}

#[cfg(windows)]
#[test]
fn human_prompt_selected_resume_transfers_only_its_own_windows_reset_generation() {
    use crate::tests::delegation_process_cleanup::ProductionWorkerTree;
    let temp = TestTempRoot::create("selected-human-dedicated-reset");
    let mut state = test_app_state();
    state.agent_runtime_spawning_enabled = true;
    let session = test_session_id(&state, Agent::Claude);
    let tree = ProductionWorkerTree::spawn(&state, &session, temp.path(), false);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        record.session.status = SessionStatus::Idle;
        record.runtime_reset_required = true;
        record.set_auto_dispatch_blocked(true);
        queue_prompt_on_record_with_source(
            record,
            PendingPrompt {
                id: "selected-reset-human".into(),
                timestamp: stamp_now(),
                text: "selected original".into(),
                expanded_text: None,
                source: None,
                attachments: vec![],
                engram_interrupted: false,
                is_engram_retained: false,
            },
            vec![],
            QueuedPromptSource::Human,
        );
        inner.stamp_session_at_index(index);
        state.commit_locked(&mut inner).unwrap();
    }
    let selection = selected_head(&state, &session);
    // The real cleanup must pass selected-owner revalidation before this
    // genuine replacement launch failure is reached, rather than stale-owner
    // rejection after the cleanup increments its own Stop generation.
    state.agent_runtime_spawning_enabled = false;
    let (claimed_tx, claimed_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *tree.tree.cleanup_gate.lock().unwrap() = Some(TestStopFenceGate {
        claimed_tx,
        release_rx,
    });
    let worker_state = state.clone();
    let worker_session = session.clone();
    let worker = std::thread::spawn(move || {
        worker_state.resume_selected_human_prompt(&worker_session, selection)
    });
    phase_sync::receive(&claimed_rx, "selected Human reaches owned off-lock reset");
    assert!(state.inner.inner.try_lock().is_ok());
    release_tx.send(()).unwrap();
    let error = worker
        .join()
        .unwrap()
        .err()
        .expect("replacement launch remains disabled");
    assert!(
        error.message.contains("agent runtime spawning is disabled"),
        "{}",
        error.message
    );
    tree.await_exit();
    tree.assert_peers_exited();
    assert!(tree.tree.is_confirmed());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(!record.runtime_stop_in_progress);
    assert!(!record.runtime_reset_required);
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(
        record.queued_prompts[0].pending_prompt.id,
        "selected-reset-human"
    );
    assert!(record.human_prompt_turn.is_none());
}

#[cfg(windows)]
#[test]
fn human_prompt_windows_reset_reaches_the_existing_off_lock_cleanup_owner() {
    use crate::tests::delegation_process_cleanup::ProductionWorkerTree;
    let temp = TestTempRoot::create("human-dedicated-reset");
    let mut state = test_app_state();
    state.agent_runtime_spawning_enabled = true;
    let session = test_session_id(&state, Agent::Claude);
    let owner = ProductionWorkerTree::spawn(&state, &session, temp.path(), false);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].session.status = SessionStatus::Idle;
        inner.sessions[index].runtime_reset_required = true;
    }
    // Only the replacement launch is disabled. The old real worker tree must
    // still be retired by the same off-lock owner used by ordinary dispatch.
    state.agent_runtime_spawning_enabled = false;
    let (claimed_tx, claimed_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *owner.tree.cleanup_gate.lock().unwrap() = Some(TestStopFenceGate {
        claimed_tx,
        release_rx,
    });
    let worker_state = state.clone();
    let worker_session = session.clone();
    let worker =
        std::thread::spawn(move || worker_state.dispatch_human_turn(&worker_session, prompt()));
    phase_sync::receive(&claimed_rx, "human promotion reaches real off-lock reset");
    assert!(
        state.inner.inner.try_lock().is_ok(),
        "cleanup cannot wait under StateInner"
    );
    release_tx.send(()).unwrap();
    let result = worker.join().unwrap();
    assert!(
        matches!(result.unwrap(), DispatchTurnResult::Queued),
        "accepted queue survives replacement startup failure without a false unsent error"
    );
    owner.await_exit();
    owner.assert_peers_exited();
    assert!(owner.tree.is_confirmed());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(!record.runtime_stop_in_progress);
    assert!(!record.runtime_reset_required);
    assert_eq!(record.session.status, SessionStatus::Error);
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(record.session.preview.contains("start failed"));
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(record.queued_prompts[0].source, QueuedPromptSource::Human);
}

#[tokio::test]
async fn human_prompt_provider_start_failure_is_accepted_and_resume_reuses_one_original_envelope() {
    let (mut state, session, _old_receiver) = ordinary_selection_fixture();
    state.agent_runtime_spawning_enabled = false;
    install_single_wire_codex_fixture(&state, None);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].queued_prompts.clear();
        inner.sessions[index].set_auto_dispatch_blocked(false);
    }
    let response = app_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{session}/messages"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"text":"run this exactly once", "origin":"human"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let original_id = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.session.status, SessionStatus::Error);
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(
            record
                .session
                .preview
                .contains("agent runtime spawning is disabled")
        );
        assert_eq!(record.queued_prompts.len(), 1);
        let head = &record.queued_prompts[0];
        assert_eq!(head.source, QueuedPromptSource::Human);
        assert_eq!(head.pending_prompt.text, "run this exactly once");
        assert!(head.promoted_message_index.is_none() && !head.has_engram_intent());
        assert!(record.human_prompt_turn.is_none());
        head.pending_prompt.id.clone()
    };
    let (runtime, receiver, _process) = test_shared_codex_runtime("retry-accepted-human-start");
    install_single_wire_codex_fixture(&state, Some(runtime));
    state.resume_session_queue(&session).unwrap();
    assert!(
        matches!(receiver.try_recv().unwrap(), CodexRuntimeCommand::Prompt { command, .. }
        if command.prompt.contains("run this exactly once"))
    );
    state.resume_session_queue(&session).unwrap();
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(record.queued_prompts.is_empty());
    assert_eq!(
        record.human_prompt_turn.as_ref().unwrap().prompt_id,
        original_id
    );
}

#[test]
fn human_prompt_failed_or_expired_promotion_ack_cannot_strand_or_replace_a_successor() {
    for expires in [false, true] {
        let (mut state, _project, session, receiver, transport) = queue::held_fixture();
        enqueue_human_for_resume(&state, &session, false);
        let duties =
            serde_json::to_value(&state.inner.lock().unwrap().engram_session_obligations).unwrap();
        state.shutdown_persist_blocking();
        let (tx, rx) = mpsc::channel();
        state.persist_tx = tx;
        let worker_state = state.clone();
        let worker_session = session.clone();
        let worker =
            std::thread::spawn(move || worker_state.start_human_queued_turn(&worker_session, true));
        let fence = loop {
            if let PersistRequest::Fence(fence) =
                phase_sync::receive(&rx, "human promotion's exact image ACK")
            {
                assert!(matches!(
                    &fence.target,
                    PersistFenceTarget::EngramProjectContainment(_)
                ));
                break fence;
            }
        };
        assert!(receiver.try_recv().is_err());
        let clock = state.engram_budget_clock();
        let late = if expires {
            clock.wait_for_scripted_waiter();
            clock.advance(
                fence
                    .completion
                    .deadline
                    .saturating_duration_since(clock.now()),
            );
            Some(fence)
        } else {
            fence.finish(Err(PersistFenceError::WriteFailed(
                "scripted rejected promotion row".into(),
            )));
            None
        };
        assert!(worker.join().unwrap().is_err());
        {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert_eq!(record.session.status, SessionStatus::Error);
            assert!(record.queued_prompts.iter().any(|head| head.source
                == QueuedPromptSource::Human
                && head.engram_ordinary_handoff));
            assert_eq!(
                serde_json::to_value(&inner.engram_session_obligations).unwrap(),
                duties
            );
        }
        // The real writer was joined before installing this fake receiver.
        // Dropping it restores synchronous fallback without an orphan writer
        // or changing evidence or disposition of the earlier attempt.
        drop(rx);
        let mut fresh = prompt();
        fresh.text = "Actual successor after failed promotion ACK".into();
        let dispatch = match state.dispatch_human_turn(&session, fresh).unwrap() {
            DispatchTurnResult::Dispatched(d) | DispatchTurnResult::DispatchedAfterQueue(d) => d,
            DispatchTurnResult::Queued => panic!("failed exact ACK cannot strand a new human turn"),
        };
        let generation = dispatch.active_turn_generation();
        assert!(matches!(
            deliver_turn_dispatch(&state, dispatch),
            TurnDispatchDeliveryOutcome::Delivered
        ));
        assert!(
            matches!(receiver.try_recv().unwrap(), CodexRuntimeCommand::Prompt { command, .. }
            if command.prompt.contains("Actual successor after failed promotion ACK"))
        );
        if let Some(late) = late {
            late.finish(Ok(()));
        }
        assert!(receiver.try_recv().is_err());
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.session.status, SessionStatus::Active);
        assert_eq!(record.active_turn_generation, generation);
        assert_eq!(
            serde_json::to_value(&inner.engram_session_obligations).unwrap(),
            duties
        );
        assert_eq!(transport.requests().len(), 2);
    }
}

#[test]
fn human_prompt_orchestrator_destination_keeps_template_rendering() {
    for expanded in [false, true] {
        let (state, project, session, root, receiver) = fixture();
        let mut instance = sample_remote_orchestrator_state(
            &project,
            &root.to_string_lossy(),
            1,
            OrchestratorInstanceStatus::Running,
        )
        .orchestrators
        .remove(0);
        instance.session_instances[0].session_id = session.clone();
        let template_id = instance.session_instances[0].template_session_id.clone();
        instance
            .template_snapshot
            .sessions
            .iter_mut()
            .find(|entry| entry.id == template_id)
            .unwrap()
            .instructions = "KEEP THIS ORCHESTRATOR ROLE".into();
        state
            .inner
            .lock()
            .unwrap()
            .orchestrator_instances
            .push(instance);
        let mut request = prompt();
        request.text = "human destination input".into();
        if expanded {
            request.expanded_text = Some("expanded human destination input".into());
        }
        let dispatch = match state.dispatch_human_turn(&session, request).unwrap() {
            DispatchTurnResult::Dispatched(d) | DispatchTurnResult::DispatchedAfterQueue(d) => d,
            DispatchTurnResult::Queued => panic!("idle orchestrator human input must start"),
        };
        assert!(matches!(
            deliver_turn_dispatch(&state, dispatch),
            TurnDispatchDeliveryOutcome::Delivered
        ));
        assert!(
            matches!(receiver.try_recv().unwrap(), CodexRuntimeCommand::Prompt { command, .. }
            if command.prompt.contains("KEEP THIS ORCHESTRATOR ROLE")
                && command.prompt.contains(if expanded { "expanded human destination input" } else { "human destination input" }))
        );
    }
}

#[test]
fn human_prompt_unknown_tail_does_not_preempt_an_ordinary_head() {
    let (state, receiver) =
        test_app_state_with_delegation_codex_runtime("ordinary-before-unknown-human");
    let session = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.session.status = SessionStatus::Idle;
        for (id, source, unknown) in [
            ("ordinary-head", QueuedPromptSource::User, false),
            ("unknown-human-tail", QueuedPromptSource::Human, true),
        ] {
            queue_prompt_on_record_with_source(
                record,
                PendingPrompt {
                    id: id.into(),
                    timestamp: stamp_now(),
                    text: id.into(),
                    expanded_text: None,
                    source: None,
                    attachments: vec![],
                    engram_interrupted: false,
                    is_engram_retained: false,
                },
                vec![],
                source,
            );
            record
                .queued_prompts
                .back_mut()
                .unwrap()
                .engram_ordinary_handoff = unknown;
        }
        sync_pending_prompts(record);
        state.commit_locked(&mut inner).unwrap();
    }
    let dispatch = state
        .dispatch_next_queued_turn(&session, false)
        .unwrap()
        .expect("ordinary head remains reachable");
    assert!(dispatch.human_prompt_id().is_none());
    assert!(matches!(
        deliver_turn_dispatch(&state, dispatch),
        TurnDispatchDeliveryOutcome::Delivered
    ));
    assert!(
        matches!(receiver.try_recv().unwrap(), CodexRuntimeCommand::Prompt { command, .. }
        if command.prompt.contains("ordinary-head"))
    );
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(
        record
            .queued_prompts
            .iter()
            .any(|head| head.pending_prompt.id == "unknown-human-tail"
                && head.engram_ordinary_handoff)
    );
}

#[test]
fn human_prompt_old_obligation_does_not_allow_skipping_unrelated_user_fifo() {
    let (state, _project, session, receiver, _transport) = queue::held_fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        queue_prompt_on_record(
            &mut inner.sessions[index],
            PendingPrompt {
                id: "independent-user-head".into(),
                timestamp: stamp_now(),
                text: "earlier independent user".into(),
                expanded_text: None,
                source: None,
                attachments: vec![],
                engram_interrupted: false,
                is_engram_retained: false,
            },
            vec![],
        );
        state.commit_locked(&mut inner).unwrap();
    }
    let dispatch = match state.dispatch_human_turn(&session, prompt()).unwrap() {
        DispatchTurnResult::Dispatched(dispatch) | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("OFF must activate the earlier eligible ordinary prompt"),
    };
    assert!(matches!(deliver_turn_dispatch(&state, dispatch), TurnDispatchDeliveryOutcome::Delivered));
    assert!(matches!(phase_sync::receive(&receiver, "the earlier ordinary FIFO prompt"),
        CodexRuntimeCommand::Prompt { command, .. } if command.prompt.contains("earlier independent user")));
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.queued_prompts[0].pending_prompt.id, "original-held-head");
    assert_eq!(record.queued_prompts[1].source, QueuedPromptSource::Human);
}
