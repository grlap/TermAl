//! Ordinary admission availability and lazy boot contracts, mounted by
//! engram_root_dispatch.rs. Uses the existing root transport and provider channel.
use super::*;

#[path = "engram_ordinary_admission_regressions.rs"]
mod ordinary_regressions;

#[path = "engram_ordinary_boundary_regressions.rs"]
mod ordinary_boundaries;

#[path = "prompt_admission_observations.rs"]
mod prompt_observations;

fn inspect<T>(state: &AppState, session: &str, read: impl FnOnce(&SessionRecord) -> T) -> T {
    let inner = state.inner.lock().unwrap();
    read(&inner.sessions[inner.find_session_index(session).unwrap()])
}

fn assert_ungranted_delivery(source: QueuedPromptSource, fail_bind: bool) {
    let unavailable = ScriptedEngramControlResponse::Reply(Err(
        EngramTransportError::spawn_failed("contract: control process did not start"),
    ));
    let replies = if fail_bind {
        vec![unavailable]
    } else {
        vec![bind_reply("available-binding"), unavailable]
    };
    let (state, session, receiver, transport) = root_fixture(replies);
    let text = "ordinary prompt survives unavailable optional admission";
    queue_test_engram_prompt(&state, &session, text, source, None);
    let started = state
        .start_next_queued_turn_off_lock(&session, false, false)
        .unwrap();
    if let Some(started) = started {
        let _ = deliver_turn_dispatch(&state, started.dispatch);
    }
    let received = receiver.try_recv();
    assert!(
        matches!(&received, Ok(CodexRuntimeCommand::Prompt { session_id, command })
            if session_id == &session && command.prompt.contains(text)),
        "certified pre-Begin unavailability must deliver this ordinary prompt"
    );
    assert!(receiver.try_recv().is_err(), "exactly one provider command");
    let (grant, uncertain, retries, queue_len, cards, token) =
        inspect(&state, &session, |record| {
            (
                record.engram.active_grant_id.clone(),
                record.engram.uncertain_grant_id.clone(),
                record.engram.admission_retry.is_some()
                    || record.engram.bind_retry.is_some()
                    || record.engram.abort_retry.is_some(),
                record.queued_prompts.len(),
                record
                    .session
                    .messages
                    .iter()
                    .filter_map(|message| match message {
                        Message::EngramControl { card, .. } => Some(card.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                record.runtime.runtime_token().unwrap(),
            )
        });
    assert!(grant.is_none() && uncertain.is_none());
    assert!(
        !retries,
        "ordinary degraded dispatch must not arm automatic admission"
    );
    assert_eq!(queue_len, 0);
    let card = cards.last().expect("degraded dispatch is explained");
    assert_eq!(card.decision, EngramControlCardDecision::Degraded);
    assert_eq!(card.dispatch, EngramControlCardDispatch::SentWithoutGrant);
    assert!(card.grant_id.is_none());
    let cause = card
        .causal_failure
        .as_ref()
        .expect("original cause is retained");
    assert_eq!(
        cause.remote_application,
        EngramRemoteApplication::NotStarted
    );
    assert_eq!(
        cause.operation,
        if fail_bind {
            "session_bind"
        } else {
            "turn_evaluate"
        }
    );
    assert_eq!(
        operations(&transport),
        if fail_bind {
            vec!["session_bind".to_owned()]
        } else {
            vec!["session_bind".to_owned(), "turn_evaluate".to_owned()]
        }
    );
    let workdir = inspect(&state, &session, |record| record.session.workdir.clone());
    // Recorder input is a fixture observation, not an executed test. Even a
    // recognized command cannot create a credited check for this ungranted turn.
    state.note_engram_command_started(
        &session,
        &EngramObservationProvenance::Ambient,
        "ordinary-ungranted-check",
        Some("cargo test --bin termal"),
        Some(&workdir),
    );
    assert!(inspect(&state, &session, |record| record
        .engram
        .active_turn_checks
        .is_empty()));
    state
        .finish_turn_ok_if_runtime_matches(&session, &token)
        .unwrap();
    assert!(
        !operations(&transport)
            .iter()
            .any(|op| op == "turn_checkpoint")
    );
}

#[test]
fn admission_contract_user_bind_outage_delivers_without_grant() {
    assert_ungranted_delivery(QueuedPromptSource::User, true);
}

#[test]
fn admission_contract_mailbox_evaluate_outage_delivers_without_grant() {
    assert_ungranted_delivery(QueuedPromptSource::Mailbox, false);
}

#[test]
fn admission_contract_unknown_application_retains_exact_bind_or_evaluate() {
    for source in [QueuedPromptSource::User, QueuedPromptSource::Mailbox] {
        for fail_bind in [true, false] {
            let lost = ScriptedEngramControlResponse::Reply(Err(EngramTransportError::transport(
                "response lost after the request may have applied",
            )));
            let responses = if fail_bind {
                vec![
                    lost,
                    bind_reply("replayed-token"),
                    grant_reply("replayed-grant"),
                    begin_reply("replayed-grant"),
                ]
            } else {
                vec![
                    bind_reply("replayed-token"),
                    lost,
                    grant_reply("replayed-grant"),
                    begin_reply("replayed-grant"),
                ]
            };
            let (state, session, receiver, transport) = root_fixture(responses);
            queue_test_engram_prompt(&state, &session, "uncertain admission", source, None);
            let started = state
                .start_next_queued_turn_off_lock(&session, false, false)
                .unwrap()
                .unwrap();
            deliver_turn_dispatch(&state, started.dispatch).unwrap();
            assert!(
                receiver.try_recv().is_err(),
                "Unknown is not certified availability"
            );
            let before = inspect(&state, &session, |record| {
                let head = record.queued_prompts.front().unwrap();
                assert!(!head.engram_ordinary_handoff);
                assert!(head.has_engram_intent());
                assert!(record.engram.active_grant_id.is_none());
                head.pending_prompt.clone()
            });
            let failed_request = transport.requests().last().unwrap().request.clone();
            elapse_bind_backoff(&state, &session);
            state.resume_session_queue(&session).unwrap();
            assert!(
                matches!(receiver.try_recv().unwrap(), CodexRuntimeCommand::Prompt { command, .. }
                if command.prompt.contains(&before.text))
            );
            assert!(
                receiver.try_recv().is_err(),
                "only the accepted Begin permits one handoff"
            );
            let requests = transport.requests();
            let replay_index = if fail_bind { 1 } else { 2 };
            assert_eq!(
                requests[replay_index].request, failed_request,
                "the exact uncertain request is replayed"
            );
            assert_eq!(requests.last().unwrap().request["operation"], "turn_begin");
        }
    }
}

#[test]
fn admission_contract_lost_evaluate_cannot_bypass_replayed_policy_decision() {
    for decision in ["refuse", "defer"] {
        let policy = if decision == "refuse" {
            evaluation_refusal_reply("policy_denied")
        } else {
            defer_reply("busy")
        };
        let (state, session, receiver, transport) = root_fixture([
            bind_reply("token"),
            ScriptedEngramControlResponse::Reply(Err(EngramTransportError::transport(
                "evaluation response lost",
            ))),
            policy,
        ]);
        deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
        assert!(receiver.try_recv().is_err());
        let resumed = state.resume_session_queue(&session);
        if decision == "refuse" {
            assert!(
                matches!(resumed, Err(error) if error.status == StatusCode::CONFLICT),
                "a replayed refusal is reported as a public conflict"
            );
        } else {
            resumed.expect("a replayed defer parks the head without a provider handoff");
        }
        let recorded = inspect(&state, &session, |record| {
            record
                .session
                .messages
                .iter()
                .rev()
                .find_map(|message| match message {
                    Message::EngramControl { card, .. } => Some(card.decision),
                    _ => None,
                })
        });
        assert_eq!(
            recorded,
            Some(if decision == "refuse" {
                EngramControlCardDecision::Refuse
            } else {
                EngramControlCardDecision::Defer
            })
        );
        assert!(
            receiver.try_recv().is_err(),
            "the replayed policy decision withholds delivery"
        );
        let requests = transport.requests();
        assert_eq!(requests[1].request, requests[2].request);
        assert!(
            !requests
                .iter()
                .any(|request| request.request["operation"] == "turn_begin")
        );
        assert!(!inspect(&state, &session, |record| record
            .queued_prompts
            .iter()
            .any(|head| head.engram_ordinary_handoff)));
    }
}

#[test]
fn admission_contract_local_and_protocol_faults_never_fall_open() {
    let mut retirement = EngramTransportError::local_state("issued grant retirement is pending");
    retirement.code = Some("issued_grant_retirement_pending".into());
    for error in [
        retirement,
        EngramTransportError::local_state("local store write failed"),
        EngramTransportError::protocol("invalid control response"),
        EngramTransportError::remote(EngramControlErrorBody {
            code: "task_access_denied".into(),
            message: "policy refused".into(),
        }),
    ] {
        let (state, session, receiver, _) = root_fixture([
            bind_reply("binding"),
            ScriptedEngramControlResponse::Reply(Err(error)),
        ]);
        let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
        assert!(receiver.try_recv().is_err());
        assert!(!inspect(&state, &session, |record| record
            .queued_prompts
            .iter()
            .any(|head| head.engram_ordinary_handoff)));
    }
}

#[test]
fn admission_contract_unknown_safety_marker_and_begin_phase_are_not_absence() {
    let (state, session, _, _) = root_fixture([]);
    queue_test_engram_prompt(
        &state,
        &session,
        "held work",
        QueuedPromptSource::User,
        None,
    );
    let mut value = inspect(&state, &session, |record| {
        serde_json::to_value(record.queued_prompts.front().unwrap()).unwrap()
    });
    value["engram_ordinary_handoff"] = json!("future-phase");
    assert!(serde_json::from_value::<QueuedPromptRecord>(value).is_err());
    assert!(
        serde_json::from_value::<EngramPreparedBegin>(json!({
            "idempotency_key":"retained-key", "grant_id":"retained-grant",
            "delivery_tokens":[], "issued_no_later_than":"2026-01-01T00:00:00Z",
            "phase":"future-phase"
        }))
        .is_err()
    );
}

#[test]
fn admission_contract_saved_ordinary_handoff_cannot_replay_after_restart() {
    let (state, session, receiver, transport) =
        root_fixture([ScriptedEngramControlResponse::Reply(Err(
            EngramTransportError::spawn_failed("unavailable"),
        ))]);
    let dispatch = root_dispatch(&state, &session, false);
    let generation = dispatch.engram_dispatch_generation().unwrap();
    assert_eq!(
        state.prepare_engram_turn_delivery_off_lock(&session, generation),
        EngramTurnDeliveryPreparation::Ready
    );
    assert!(inspect(&state, &session, engram_provider_handoff_allowed));
    let encoded = inspect(&state, &session, |record| {
        serde_json::to_string(&PersistedSessionRecord::from_record(record)).unwrap()
    });
    let restored: PersistedSessionRecord = serde_json::from_str(&encoded).unwrap();
    let restored = restored.into_record().unwrap();
    assert!(
        restored
            .queued_prompts
            .front()
            .unwrap()
            .engram_ordinary_handoff
    );
    assert!(restored.queued_prompts.front().unwrap().engram_interrupted);
    assert!(restored.session.queue_paused);
    assert!(restored.engram.ordinary_handoff.is_none());
    assert!(!engram_provider_handoff_allowed(&restored));
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index] = restored;
    }
    assert!(
        state.resume_session_queue(&session).is_err(),
        "delivery is unknown, not replayable"
    );
    assert!(
        state
            .start_next_queued_turn_off_lock(&session, false, false)
            .unwrap()
            .is_none()
    );
    assert!(receiver.try_recv().is_err());
    assert_eq!(operations(&transport), ["session_bind"]);
    drop(dispatch);
}

#[test]
fn admission_contract_policy_and_workflow_still_withhold() {
    for decision in ["refuse", "defer", "workflow"] {
        let responses = match decision {
            "refuse" => vec![
                bind_reply("token"),
                evaluation_refusal_reply("policy_denied"),
            ],
            "defer" => vec![bind_reply("token"), defer_reply("not_ready")],
            _ => vec![ScriptedEngramControlResponse::Reply(Err(
                EngramTransportError::spawn_failed("workflow control unavailable"),
            ))],
        };
        let (state, session, receiver, transport) = root_fixture(responses);
        let source = if decision == "workflow" {
            QueuedPromptSource::Orchestrator
        } else {
            QueuedPromptSource::User
        };
        queue_test_engram_prompt(&state, &session, "strict admission", source, None);
        if let Some(started) = state
            .start_next_queued_turn_off_lock(&session, false, false)
            .unwrap()
        {
            let _ = deliver_turn_dispatch(&state, started.dispatch);
        }
        assert!(
            receiver.try_recv().is_err(),
            "{decision} is never availability permission"
        );
        assert!(!operations(&transport).iter().any(|op| op == "turn_begin"));
    }
}

#[test]
fn admission_contract_boot_defers_only_settled_idle_roots() {
    let (state, session, _, transport) = root_fixture([]);
    let (project, root) = inspect(&state, &session, |record| {
        (
            record.session.project_id.clone().unwrap(),
            PathBuf::from(&record.session.workdir),
        )
    });
    let mut eager = Vec::new();
    let mut lazy = Vec::new();
    for kind in ["token", "rebind", "active", "uncertain", "legacy", "queued"] {
        let id = create_test_project_session(&state, Agent::Codex, &project, &root);
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&id).unwrap();
            let record = &mut inner.sessions[index];
            record.engram.begins_recorded = kind != "legacy";
            record.engram.routing_token = Some(format!("saved-{kind}"));
            record.engram.rebind_required = true;
            if kind == "active" {
                record.engram.active_grant_id = Some("open-grant".into());
            }
            if kind == "uncertain" {
                record.engram.uncertain_grant_id = Some("unknown-grant".into());
            }
        }
        if kind == "queued" {
            queue_test_engram_prompt(&state, &id, "saved work", QueuedPromptSource::User, None);
        }
        if matches!(kind, "token" | "rebind") {
            lazy.push(id);
        } else {
            eager.push(id);
        }
    }
    let plan = state.prepare_engram_sessions_for_boot_recovery().unwrap();
    let targets = plan
        .targets
        .iter()
        .map(|t| t.connection.session_id.clone())
        .collect::<Vec<_>>();
    for id in lazy {
        assert!(
            !targets.contains(&id),
            "a settled idle root needs no boot RPC"
        );
        assert!(!inspect(&state, &id, |r| r.engram_boot_recovery_pending));
    }
    for id in eager {
        assert!(targets.contains(&id), "unsettled work keeps eager recovery");
    }
    assert!(transport.requests().is_empty(), "selection is local");
}

#[test]
fn admission_contract_lazy_root_rebinds_before_first_turn() {
    let (state, session, receiver, transport) = root_fixture([
        status_reply("ready"),
        bind_reply("new-token"),
        grant_reply("new-grant"),
        begin_reply("new-grant"),
    ]);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        record.engram.begins_recorded = true;
        record.engram.routing_token = Some("saved-token".into());
        record.engram.rebind_required = true;
    }
    let plan = state.prepare_engram_sessions_for_boot_recovery().unwrap();
    assert!(
        plan.targets.is_empty(),
        "idle root waits for its first admission"
    );
    assert!(transport.requests().is_empty());
    let dispatch = root_dispatch(&state, &session, false);
    deliver_turn_dispatch(&state, dispatch).unwrap();
    assert!(matches!(
        receiver.try_recv(),
        Ok(CodexRuntimeCommand::Prompt { .. })
    ));
    assert!(receiver.try_recv().is_err());
    assert_eq!(
        operations(&transport),
        [
            "session_status",
            "session_bind",
            "turn_evaluate",
            "turn_begin"
        ]
    );
    assert_eq!(
        transport.requests()[0].request["routing_token"],
        "saved-token"
    );
    assert_eq!(
        inspect(&state, &session, |r| r.engram.active_grant_id.clone()).as_deref(),
        Some("new-grant")
    );
}
