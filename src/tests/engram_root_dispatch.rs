//! Root-session coverage at the same dispatch/delivery/completion boundaries
//! exercised by the child conformance tests in engram_host_adapter.rs.
//! No provider subprocess or live Engram store is used.

use super::*;

fn root_fixture(
    responses: impl IntoIterator<Item = ScriptedEngramControlResponse>,
) -> (
    AppState,
    String,
    mpsc::Receiver<CodexRuntimeCommand>,
    Arc<ScriptedEngramControlTransport>,
) {
    let (state, receiver) = test_app_state_with_delegation_codex_runtime("engram-root-runtime");
    let root = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("root-project");
    fs::create_dir_all(&root).unwrap();
    let project = create_test_project(&state, &root, "Root control");
    let session = create_test_project_session(&state, Agent::Codex, &project, &root);
    enable_test_project_engram(&state, &project, &root);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        // This suite tests admission, not the separate advisory CLI reader.
        inner.sessions[index].engram.context_nudge_pending = false;
    }
    let transport = ScriptedEngramControlTransport::new(responses);
    state.install_control_test_transport(transport.clone());
    assert!(state.inner.lock().unwrap().delegations.is_empty());
    (state, session, receiver, transport)
}

fn root_dispatch(state: &AppState, session: &str, queued: bool) -> TurnDispatch {
    if queued {
        queue_test_engram_prompt(
            state,
            session,
            "Root controlled turn",
            QueuedPromptSource::User,
            None,
        );
        state
            .start_next_queued_turn_off_lock(session, false, false)
            .unwrap()
            .expect("queued root should reach admission")
            .dispatch
    } else {
        match state
            .dispatch_turn(
                session,
                SendMessageRequest {
                    text: "Root controlled turn".to_owned(),
                    expanded_text: None,
                    attachments: Vec::new(),
                    source_session_id: None,
                    source_mailbox: None,
                },
            )
            .expect("root should reach admission")
        {
            DispatchTurnResult::Dispatched(dispatch)
            | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
            DispatchTurnResult::Queued => panic!("idle root should dispatch"),
        }
    }
}

fn operations(transport: &ScriptedEngramControlTransport) -> Vec<String> {
    transport
        .requests()
        .iter()
        .map(|r| r.request["operation"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn root_direct_and_queued_turns_bind_evaluate_begin_and_complete() {
    for queued in [false, true] {
        let (state, session, receiver, transport) = root_fixture([
            bind_reply("root-token"),
            grant_reply("root-grant"),
            begin_reply("root-grant"),
            checkpoint_reply("root-grant"),
        ]);
        let dispatch = root_dispatch(&state, &session, queued);
        assert!(
            receiver.try_recv().is_err(),
            "evaluate alone must not deliver a prompt"
        );
        assert_eq!(operations(&transport), ["session_bind", "turn_evaluate"]);
        deliver_turn_dispatch(&state, dispatch)
            .expect("begun root turn should reach the provider channel");
        assert!(matches!(
            receiver.try_recv().unwrap(),
            CodexRuntimeCommand::Prompt { .. }
        ));
        assert!(receiver.try_recv().is_err(), "exactly one provider prompt");
        assert_eq!(
            operations(&transport),
            ["session_bind", "turn_evaluate", "turn_begin"]
        );
        let token = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert_eq!(record.engram.active_grant_id.as_deref(), Some("root-grant"));
            record.runtime.runtime_token().unwrap()
        };
        state
            .finish_turn_ok_if_runtime_matches(&session, &token)
            .unwrap();
        assert_eq!(
            operations(&transport),
            [
                "session_bind",
                "turn_evaluate",
                "turn_begin",
                "turn_checkpoint"
            ]
        );
        let requests = transport.requests();
        assert!(requests.iter().all(|r| r.connection.session_id == session));
        assert_eq!(
            requests[0].request["external_ref"],
            format!("termal:session:{session}")
        );
        assert_eq!(
            requests[0].request["mediated_effects"],
            json!(["observe", "communicate"])
        );
        assert_eq!(
            requests[1].request["requested_effects"],
            requests[0].request["mediated_effects"]
        );
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.engram.active_grant_id.is_none());
        assert!(record.session.messages.iter().any(|m| matches!(m, Message::EngramControl { card, .. }
            if card.stage == EngramControlStage::Checkpoint && card.decision == EngramControlCardDecision::Grant)));
    }
}

#[test]
fn root_direct_and_queued_denials_and_failures_never_reach_provider() {
    for queued in [false, true] {
        for failure in ["evaluate", "defer", "begin", "bind", "fatal"] {
            let responses = match failure {
                "evaluate" => vec![
                    bind_reply("root-token"),
                    evaluation_refusal_reply("policy_denied"),
                ],
                "defer" => vec![bind_reply("root-token"), defer_reply("not_ready")],
                "begin" => vec![
                    bind_reply("root-token"),
                    grant_reply("root-grant"),
                    // Begin and checkpoint share the same refusal response shape.
                    checkpoint_refusal_reply("policy_denied"),
                ],
                "bind" => vec![ScriptedEngramControlResponse::Reply(Err(
                    EngramTransportError::transport("unavailable"),
                ))],
                "fatal" => vec![],
                _ => unreachable!(),
            };
            let (state, session, receiver, transport) = root_fixture(responses);
            if failure == "fatal" {
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&session).unwrap();
                inner.sessions[index].engram.disabled_reason = Some("control_disabled".to_owned());
            }
            let dispatch = root_dispatch(&state, &session, queued);
            assert_eq!(
                deliver_turn_dispatch(&state, dispatch)
                    .expect_err("must withhold denied root prompt")
                    .status,
                StatusCode::CONFLICT
            );
            assert!(
                receiver.try_recv().is_err(),
                "queued={queued} failure={failure}"
            );
            let ops = operations(&transport);
            if failure == "fatal" {
                assert!(
                    transport.requests().is_empty(),
                    "fatal sessions make no control calls"
                );
            }
            assert_eq!(
                ops.iter().filter(|op| op.as_str() == "turn_begin").count(),
                usize::from(failure == "begin")
            );
            assert!(!ops.iter().any(|op| op == "turn_checkpoint"));
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(record.engram.active_grant_id.is_none());
            assert!(
                record
                    .session
                    .messages
                    .iter()
                    .any(|m| matches!(m, Message::EngramControl { card, .. }
                if card.dispatch == EngramControlCardDispatch::Withheld))
            );
        }
    }
}

#[test]
fn root_queued_mailbox_and_orchestrator_turns_use_admission() {
    for source in [
        QueuedPromptSource::Mailbox,
        QueuedPromptSource::Orchestrator,
    ] {
        let (state, session, receiver, transport) = root_fixture([
            bind_reply("root-token"),
            evaluation_refusal_reply("policy_denied"),
        ]);
        queue_test_engram_prompt(&state, &session, "Automatic root wake", source, None);
        let dispatch = state
            .start_next_queued_turn_off_lock(&session, false, false)
            .unwrap()
            .unwrap()
            .dispatch;
        assert_eq!(
            deliver_turn_dispatch(&state, dispatch).unwrap_err().status,
            StatusCode::CONFLICT
        );
        assert!(receiver.try_recv().is_err());
        assert_eq!(operations(&transport), ["session_bind", "turn_evaluate"]);
    }
}

#[test]
fn queued_mailbox_coalescing_retires_old_grant_and_evaluates_new_intent_for_roots_and_children() {
    for child in [false, true] {
        let (state, root, receiver, _) = root_fixture([
            bind_reply("setup-parent"),
            bind_reply("setup-child"),
            grant_reply("setup-grant"),
            begin_reply("setup-grant"),
            checkpoint_reply("setup-grant"),
        ]);
        let (session, external_ref) = if child {
            let created = state
                .create_read_only_delegation(
                    &root,
                    CreateDelegationRequest {
                        prompt: "Set up the child".into(),
                        title: None,
                        cwd: None,
                        agent: Some(Agent::Codex),
                        model: None,
                        mode: Some(DelegationMode::Reviewer),
                        write_policy: Some(DelegationWritePolicy::ReadOnly),
                    },
                )
                .unwrap();
            assert!(matches!(
                receive_synchronous_engram_prompt(&state, &receiver, "setup prompt").unwrap(),
                CodexRuntimeCommand::Prompt { .. }
            ));
            let session = created.delegation.child_session_id;
            let token = {
                let inner = state.inner.lock().expect("state mutex poisoned");
                inner.sessions[inner.find_session_index(&session).unwrap()]
                    .runtime
                    .runtime_token()
                    .unwrap()
            };
            state
                .finish_turn_ok_if_runtime_matches(&session, &token)
                .unwrap();
            (
                session,
                format!("termal:delegation:{}", created.delegation.id),
            )
        } else {
            (root.clone(), format!("termal:session:{root}"))
        };
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].engram.routing_token = Some("old-token".into());
            inner.sessions[index].engram.context_nudge_pending = false;
        }
        let source = |sequence| {
            Some(MessageSource::mailbox(
                root.clone(),
                "Sender".into(),
                MailboxMessageSource {
                    mailbox_id: "coalesced-mailbox".into(),
                    message_id: format!("mail-{sequence}"),
                    sequence,
                    unread_count: sequence,
                },
            ))
        };
        queue_test_engram_prompt(
            &state,
            &session,
            "Sequence 1",
            QueuedPromptSource::Mailbox,
            source(1),
        );
        let (evaluate, gate) = gated_engram_step("turn_evaluate", grant_reply("obsolete-grant"));
        let transport = GatedEngramControlTransport::new([
            evaluate,
            immediate_engram_step(
                "session_status",
                ScriptedEngramControlResponse::Reply(Ok(json!({
                    "phase": "ready", "open_grant_id": "obsolete-grant"
                }))),
            ),
            immediate_engram_step(
                "turn_checkpoint",
                checkpoint_refusal_reply("grant_not_begun"),
            ),
            immediate_engram_step("session_bind", rebind_reply("new-token")),
            immediate_engram_step("turn_evaluate", grant_reply("current-grant")),
            immediate_engram_step("turn_begin", begin_reply("current-grant")),
            immediate_engram_step("turn_checkpoint", checkpoint_reply("current-grant")),
        ]);
        state.install_control_test_transport(transport.clone());
        let dispatch = std::thread::scope(|scope| {
            let worker =
                scope.spawn(|| state.start_next_queued_turn_off_lock(&session, false, false));
            let evaluated = gate.wait();
            let new_fingerprint = {
                let mut inner = state.inner.lock().expect("state mutex poisoned");
                let index = inner.find_session_index(&session).unwrap();
                let record = &mut inner.sessions[index];
                let generation = record.engram.dispatch_generation;
                let queued = record.queued_prompts.front_mut().unwrap();
                let id = queued.pending_prompt.id.clone();
                // Isolate the exact in-place mutation made by mailbox coalescing;
                // queue identity and dispatch generation deliberately stay put.
                queued.pending_prompt.text = "Sequence 2".into();
                queued.pending_prompt.source = source(2);
                let fingerprint = engram_turn_intent_fingerprint(
                    &queued.pending_prompt.text,
                    None,
                    &queued.attachments,
                    queued.pending_prompt.source.as_ref(),
                    queued.source,
                );
                assert_eq!(queued.pending_prompt.id, id);
                assert_eq!(record.engram.dispatch_generation, generation);
                state.commit_locked(&mut inner).unwrap();
                fingerprint
            };
            assert_ne!(evaluated.request["intent_fingerprint"], new_fingerprint);
            assert!(receiver.try_recv().is_err());
            gate.release();
            let started = worker.join().unwrap().unwrap().unwrap();
            let requests = transport.requests();
            assert_eq!(requests.len(), 5, "repair must precede fresh evaluation");
            assert_eq!(requests[4].request["intent_fingerprint"], new_fingerprint);
            assert_ne!(
                requests[0].request["idempotency_key"],
                requests[4].request["idempotency_key"]
            );
            assert_eq!(requests[3].request["external_ref"], external_ref);
            started.dispatch
        });
        deliver_turn_dispatch(&state, dispatch).unwrap();
        assert!(matches!(
            receiver.try_recv().unwrap(),
            CodexRuntimeCommand::Prompt { .. }
        ));
        assert!(
            receiver.try_recv().is_err(),
            "only the newly admitted prompt is delivered"
        );
        let token = {
            let inner = state.inner.lock().expect("state mutex poisoned");
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert_eq!(
                record.engram.active_grant_id.as_deref(),
                Some("current-grant")
            );
            assert!(record.queued_prompts.is_empty());
            record.runtime.runtime_token().unwrap()
        };
        state
            .finish_turn_ok_if_runtime_matches(&session, &token)
            .unwrap();
        let requests = transport.requests();
        let begins: Vec<_> = requests
            .iter()
            .filter(|r| r.request["operation"] == "turn_begin")
            .collect();
        assert_eq!(begins.len(), 1);
        assert_eq!(begins[0].request["grant_id"], "current-grant");
    }
}

#[test]
fn root_completion_drains_a_busy_queued_prompt_through_fresh_admission() {
    for allow_followup in [false, true] {
        let mut responses = vec![
            bind_reply("root-token"),
            grant_reply("first"),
            begin_reply("first"),
            checkpoint_reply("first"),
        ];
        if allow_followup {
            responses.extend([
                grant_reply("second"),
                begin_reply("second"),
                checkpoint_reply("second"),
            ]);
        } else {
            responses.push(evaluation_refusal_reply("policy_denied"));
        }
        let (state, session, receiver, transport) = root_fixture(responses);
        let first = root_dispatch(&state, &session, false);
        deliver_turn_dispatch(&state, first).unwrap();
        assert!(matches!(
            receiver.try_recv().unwrap(),
            CodexRuntimeCommand::Prompt { .. }
        ));
        let queued = state
            .dispatch_turn(
                &session,
                SendMessageRequest {
                    text: "Queued while busy".to_owned(),
                    expanded_text: None,
                    attachments: Vec::new(),
                    source_session_id: None,
                    source_mailbox: None,
                },
            )
            .unwrap();
        assert!(matches!(queued, DispatchTurnResult::Queued));
        assert_eq!(
            operations(&transport),
            ["session_bind", "turn_evaluate", "turn_begin"]
        );
        let token = {
            let inner = state.inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&session).unwrap()]
                .runtime
                .runtime_token()
                .unwrap()
        };
        let completion = state.finish_turn_ok_if_runtime_matches(&session, &token);
        if allow_followup {
            completion.expect("allowed queued turn should drain");
            assert!(matches!(
                receiver.try_recv().unwrap(),
                CodexRuntimeCommand::Prompt { .. }
            ));
            state
                .finish_turn_ok_if_runtime_matches(&session, &token)
                .unwrap();
            assert_eq!(
                operations(&transport),
                [
                    "session_bind",
                    "turn_evaluate",
                    "turn_begin",
                    "turn_checkpoint",
                    "turn_evaluate",
                    "turn_begin",
                    "turn_checkpoint"
                ]
            );
        } else {
            assert!(
                completion
                    .unwrap_err()
                    .to_string()
                    .contains("Engram did not authorize")
            );
            assert!(
                receiver.try_recv().is_err(),
                "denied queued turn never reaches provider"
            );
            assert_eq!(
                operations(&transport),
                [
                    "session_bind",
                    "turn_evaluate",
                    "turn_begin",
                    "turn_checkpoint",
                    "turn_evaluate"
                ]
            );
        }
    }
}

#[test]
fn stop_does_not_promote_a_mailbox_head_changed_during_successor_evaluation() {
    let (state, session, receiver, _) = root_fixture([]);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram.routing_token = Some("old-token".into());
        // Exercise Stop's missing-runtime recovery of a running session.
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    let source = |sequence| {
        Some(MessageSource::mailbox(
            "sender".into(),
            "Sender".into(),
            MailboxMessageSource {
                mailbox_id: "stop-mailbox".into(),
                message_id: format!("mail-{sequence}"),
                sequence,
                unread_count: sequence,
            },
        ))
    };
    queue_test_engram_prompt(
        &state,
        &session,
        "Sequence 1",
        QueuedPromptSource::Mailbox,
        source(1),
    );
    let (evaluate, gate) = gated_engram_step("turn_evaluate", grant_reply("obsolete-stop-grant"));
    let transport = GatedEngramControlTransport::new([evaluate]);
    state.install_control_test_transport(transport.clone());
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            state.stop_session_with_options(
                &session,
                StopSessionOptions {
                    dispatch_queued_prompts_on_success: true,
                    pause_automatic_resumes_on_success: false,
                    orchestrator_stop_instance_id: None,
                },
            )
        });
        gate.wait();
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(&session).unwrap();
            let head = inner.sessions[index].queued_prompts.front_mut().unwrap();
            head.pending_prompt.text = "Sequence 2".into();
            head.pending_prompt.source = source(2);
            state.commit_locked(&mut inner).unwrap();
        }
        gate.release();
        worker.join().unwrap().unwrap();
    });
    assert!(
        receiver.try_recv().is_err(),
        "obsolete grant must not release a provider prompt"
    );
    assert_eq!(
        transport.requests().len(),
        1,
        "no begin for changed queue head"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(
        record.engram.rebind_required,
        "repair issued grant before next admission"
    );
    assert!(record.engram.active_grant_id.is_none());
    assert_eq!(
        record.queued_prompts.front().unwrap().pending_prompt.text,
        "Sequence 2"
    );
}

#[test]
fn root_disabled_and_base_only_projects_preserve_ordinary_dispatch() {
    for queued in [false, true] {
        for settings_mode in ["absent", "disabled", "base"] {
            let (state, session, receiver, transport) = root_fixture([]);
            {
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&session).unwrap();
                // Base context is separately covered by the context fixture.
                inner.sessions[index].engram.context_nudge_pending = false;
                let project_id = inner.sessions[index].session.project_id.clone().unwrap();
                let project = inner
                    .projects
                    .iter_mut()
                    .find(|p| p.id == project_id)
                    .unwrap();
                match settings_mode {
                    "absent" => project.engram = None,
                    "disabled" => project.engram.as_mut().unwrap().enabled = false,
                    "base" => project.engram.as_mut().unwrap().turn_gated_control = false,
                    _ => unreachable!(),
                }
            }
            let dispatch = root_dispatch(&state, &session, queued);
            deliver_turn_dispatch(&state, dispatch).unwrap();
            assert!(matches!(
                receiver.try_recv().unwrap(),
                CodexRuntimeCommand::Prompt { .. }
            ));
            assert!(transport.requests().is_empty());
        }
    }
}

#[test]
fn marked_child_with_missing_delegation_cannot_fall_back_to_root_admission() {
    let (state, session, receiver, transport) = root_fixture([]);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].session.parent_delegation_id = Some("missing-delegation".to_owned());
    }
    let dispatch = root_dispatch(&state, &session, false);
    assert_eq!(
        deliver_turn_dispatch(&state, dispatch).unwrap_err().status,
        StatusCode::CONFLICT
    );
    assert!(receiver.try_recv().is_err());
    assert!(
        transport.requests().is_empty(),
        "never bind a damaged child as a root"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.engram.consecutive_transport_failures, 0);
    assert!(record.engram.next_bind_retry_at.is_none());
    assert!(!record.engram.rebind_required);
}

#[test]
fn root_without_children_recovers_open_grant_before_its_next_turn() {
    let (state, session, receiver, transport) = root_fixture([
        ScriptedEngramControlResponse::Reply(Ok(json!({
            "phase": "executing", "open_grant_id": "pre-crash-grant"
        }))),
        checkpoint_reply("pre-crash-grant"),
        rebind_reply("recovered-root"),
        grant_reply("next-root-grant"),
        begin_reply("next-root-grant"),
        checkpoint_reply("next-root-grant"),
    ]);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram.routing_token = Some("pre-crash-token".to_owned());
        inner.sessions[index].engram.active_grant_id = Some("pre-crash-grant".to_owned());
        inner.sessions[index].engram.rebind_required = true;
    }
    let plan = state.prepare_engram_sessions_for_boot_recovery().unwrap();
    assert_eq!(plan.targets.len(), 1, "ordinary root must be a boot target");
    assert_eq!(plan.targets[0].connection.session_id, session);
    {
        let inner = state.inner.lock().unwrap();
        assert!(
            inner.sessions[inner.find_session_index(&session).unwrap()]
                .engram_boot_recovery_pending
        );
    }
    state.recover_prepared_engram_sessions_after_boot(plan);
    assert!(
        receiver.try_recv().is_err(),
        "recovery alone must not prompt"
    );
    assert_eq!(
        operations(&transport),
        ["session_status", "turn_checkpoint", "session_bind"]
    );
    let dispatch = root_dispatch(&state, &session, false);
    deliver_turn_dispatch(&state, dispatch).unwrap();
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    let token = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(!record.engram_boot_recovery_pending);
        record.runtime.runtime_token().unwrap()
    };
    state
        .finish_turn_ok_if_runtime_matches(&session, &token)
        .unwrap();
    assert_eq!(
        operations(&transport),
        [
            "session_status",
            "turn_checkpoint",
            "session_bind",
            "turn_evaluate",
            "turn_begin",
            "turn_checkpoint"
        ]
    );
}

#[test]
fn boot_plan_excludes_unused_hidden_remote_and_base_only_sessions() {
    let (state, root_session, _receiver, transport) = root_fixture([]);
    let (project, root) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&root_session).unwrap()];
        (
            record.session.project_id.clone().unwrap(),
            PathBuf::from(&record.session.workdir),
        )
    };
    let mut expected = Vec::new();
    for kind in [
        "token",
        "grant",
        "rebind",
        "hidden",
        "remote",
        "partial-remote",
        "base",
    ] {
        let id = create_test_project_session(&state, Agent::Codex, &project, &root);
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        let record = &mut inner.sessions[index];
        match kind {
            "token" => record.engram.routing_token = Some("old-token".into()),
            "grant" => record.engram.active_grant_id = Some("open-grant".into()),
            "rebind" => record.engram.rebind_required = true,
            "hidden" => {
                record.hidden = true;
                record.engram.rebind_required = true;
            }
            "remote" | "partial-remote" => {
                record.remote_id = Some("other-host".into());
                if kind == "remote" {
                    record.remote_session_id = Some("remote-session".into());
                }
                record.engram.rebind_required = true;
            }
            "base" => {
                record.engram.rebind_required = true;
                record.session.project_id = Some("base-project".into());
                let mut base = inner.find_project(&project).unwrap().clone();
                base.id = "base-project".into();
                base.engram.as_mut().unwrap().turn_gated_control = false;
                inner.projects.push(base);
            }
            _ => unreachable!(),
        }
        if matches!(kind, "token" | "grant" | "rebind") {
            expected.push(id);
        }
    }
    // Many unused sessions must not create boot work or readiness fences.
    for _ in 0..40 {
        create_test_project_session(&state, Agent::Codex, &project, &root);
    }
    let plan = state.prepare_engram_sessions_for_boot_recovery().unwrap();
    assert_eq!(
        plan.targets
            .iter()
            .map(|target| target.connection.session_id.clone())
            .collect::<Vec<_>>(),
        expected
    );
    let inner = state.inner.lock().unwrap();
    for record in &inner.sessions {
        assert_eq!(
            record.engram_boot_recovery_pending,
            expected.contains(&record.session.id)
        );
    }
    assert!(
        transport.requests().is_empty(),
        "planning performs no control I/O"
    );
}

#[test]
fn root_failed_boot_recovery_withholds_next_prompt() {
    let (state, session, receiver, transport) =
        root_fixture([remote_error_reply("unknown_control_schema")]);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram.rebind_required = true;
    }
    let plan = state.prepare_engram_sessions_for_boot_recovery().unwrap();
    assert_eq!(plan.targets.len(), 1);
    state.recover_prepared_engram_sessions_after_boot(plan);
    let dispatch = root_dispatch(&state, &session, false);
    assert_eq!(
        deliver_turn_dispatch(&state, dispatch).unwrap_err().status,
        StatusCode::CONFLICT
    );
    assert!(receiver.try_recv().is_err());
    assert_eq!(operations(&transport), ["session_bind"]);
}

#[test]
fn root_completion_checkpoints_existing_grant_after_control_is_switched_off() {
    let (state, session, receiver, transport) = root_fixture([
        bind_reply("root-token"),
        grant_reply("root-grant"),
        begin_reply("root-grant"),
        checkpoint_reply("root-grant"),
    ]);
    let dispatch = root_dispatch(&state, &session, false);
    deliver_turn_dispatch(&state, dispatch).unwrap();
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    let token = {
        let mut inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        let token = record.runtime.runtime_token().unwrap();
        let project = record.session.project_id.clone().unwrap();
        inner
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .unwrap()
            .engram
            .as_mut()
            .unwrap()
            .turn_gated_control = false;
        token
    };
    state
        .finish_turn_ok_if_runtime_matches(&session, &token)
        .unwrap();
    assert_eq!(
        operations(&transport),
        [
            "session_bind",
            "turn_evaluate",
            "turn_begin",
            "turn_checkpoint"
        ]
    );
}
