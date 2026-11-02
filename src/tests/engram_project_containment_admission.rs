// Owns exact retained-admission recovery across containment settings changes.
// Does not use live stores, authorize independent grants, or test retirement.
use super::*;

#[path = "engram_project_containment_parked_retry.rs"]
mod parked_retry;

#[path = "engram_project_containment_bind_successor.rs"]
mod bind_successor;

#[path = "engram_project_containment_begin_replay.rs"]
mod begin_replay;

#[path = "engram_project_containment_cycles.rs"]
mod cycles;

#[path = "engram_project_containment_transfer_settings.rs"]
mod transfer_settings;

#[path = "engram_project_containment_decision_persistence.rs"]
mod decision_persistence;

pub(super) fn restore_containment_test_adapter_after_load(
    state: &AppState,
    transport: Arc<dyn EngramControlTransport>,
) {
    // app_boot.rs::AppState::new_with_paths installs this runtime-only adapter
    // after load_state_for_boot (lines 427-438). The raw loader intentionally
    // supplies a default; these control fixtures use its existing transport
    // installer to provide the same persistence-directory setup and double.
    state.install_control_test_transport(transport);
    let inner = state.inner.lock().unwrap();
    let workdir = &inner.engram_host_adapter.host_workdir;
    assert!(workdir.is_absolute());
    assert!(workdir.is_dir());
    assert_eq!(
        *workdir,
        engram_host_workdir(state.persistence_path.as_path()).unwrap()
    );
}

struct LostContainmentAdmissionReply {
    inner: Arc<StatefulEngramControlTransport>,
    lose: Mutex<BTreeSet<String>>,
    state: std::sync::Weak<StateMutex<StateInner>>,
    persistence_path: PathBuf,
    transfer_seen: AtomicBool,
}

impl EngramControlTransport for LostContainmentAdmissionReply {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        let operation = serde_json::to_value(request).unwrap()["operation"]
            .as_str()
            .unwrap()
            .to_owned();
        if operation == "turn_begin" {
            let state = self.state.upgrade().unwrap();
            let inner = state.lock().unwrap();
            let entry = inner
                .engram_session_obligations
                .entries
                .iter()
                .find(|e| e.local_session_id == connection.session_id)
                .expect("original containment entry");
            assert!(containment_admission_transfer_matches(entry));
            assert!(inner.engram_confirmed_containment_ids.contains(&entry.id));
            let image = EngramProjectContainmentFence::capture(
                &inner,
                &entry.project_id,
                &[connection.session_id.clone()],
            )
            .unwrap();
            let database = rusqlite::Connection::open(&self.persistence_path).unwrap();
            assert!(
                image.is_already_durable(&database).unwrap(),
                "both the ledger transfer and exact prepared Begin precede its send"
            );
            self.transfer_seen.store(true, Ordering::SeqCst);
        }
        let response = self.inner.request(connection, request, timeout)?;
        if self.lose.lock().unwrap().remove(&operation) {
            return Err(EngramTransportError::deadline(
                "fixture lost applied admission reply",
            ));
        }
        Ok(response)
    }
    fn shutdown_session(&self, session_id: &str) {
        self.inner.shutdown_session(session_id);
    }
}

fn retained_admission(
    lost: &[&str],
) -> (
    AppState,
    String,
    String,
    PathBuf,
    mpsc::Receiver<CodexRuntimeCommand>,
    Arc<LostContainmentAdmissionReply>,
) {
    let (state, project, session, root, receiver, transport) = parked_admission(lost);
    state
        .update_project_engram_settings(&project, settings(&state, &project, false))
        .unwrap();
    (state, project, session, root, receiver, transport)
}

fn parked_admission(
    lost: &[&str],
) -> (
    AppState,
    String,
    String,
    PathBuf,
    mpsc::Receiver<CodexRuntimeCommand>,
    Arc<LostContainmentAdmissionReply>,
) {
    let (state, project, session, root, receiver) = fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.engram = EngramSessionState::default();
        record.engram.context_nudge_pending = false;
        record.session.status = SessionStatus::Idle;
        state.commit_locked(&mut inner).unwrap();
    }
    let transport = Arc::new(LostContainmentAdmissionReply {
        inner: StatefulEngramControlTransport::new(),
        lose: Mutex::new(lost.iter().map(|s| s.to_string()).collect()),
        state: Arc::downgrade(&state.inner),
        persistence_path: state.persistence_path.as_ref().clone(),
        transfer_seen: AtomicBool::new(false),
    });
    state.install_control_test_transport(transport.clone());
    let dispatch = match state.dispatch_turn(&session, prompt()).unwrap() {
        DispatchTurnResult::Dispatched(d) | DispatchTurnResult::DispatchedAfterQueue(d) => d,
        DispatchTurnResult::Queued => panic!("initial admission must run"),
    };
    deliver_turn_dispatch(&state, dispatch).unwrap();
    assert!(receiver.try_recv().is_err());
    (state, project, session, root, receiver, transport)
}

#[test]
fn human_prompt_off_resume_preserves_retained_admission_without_control_requests() {
    for lost in ["session_bind", "turn_evaluate"] {
        for cancelled in [false, true] {
            for reload in [false, true] {
                let (state, _project, session, _root, receiver, transport) =
                    retained_admission(&[lost]);
                let (head, original) = {
                    let inner = state.inner.lock().unwrap();
                    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                    assert!(engram_session_obligation_locked(&inner, &session).is_some());
                    (
                        record.queued_prompts[0].pending_prompt.id.clone(),
                        serde_json::to_value(&inner.engram_session_obligations).unwrap(),
                    )
                };
                enqueue_human_for_resume(&state, &session, reload);
                let count = transport.inner.requests().len();
                if cancelled {
                    state.cancel_queued_prompt(&session, &head).unwrap();
                }
                state
                    .resume_session_queue(&session)
                    .expect("control OFF must permit ordinary Resume while retaining the old duty");
                assert_eq!(
                    transport.inner.requests().len(),
                    count,
                    "ordinary Resume sends no Engram request"
                );
                assert!(matches!(
                    receiver.try_recv().unwrap(),
                    CodexRuntimeCommand::Prompt { .. }
                ));
                assert!(receiver.try_recv().is_err(), "one ordinary provider turn");
                let inner = state.inner.lock().unwrap();
                assert_eq!(
                    serde_json::to_value(&inner.engram_session_obligations).unwrap(),
                    original,
                    "OFF cannot erase or settle the original authority and intent"
                );
                assert!(
                    !inner
                        .find_project(&_project)
                        .unwrap()
                        .engram
                        .as_ref()
                        .unwrap()
                        .is_control_enabled()
                );
            }
        }
    }
}

#[test]
fn project_containment_retained_bind_transfers_only_at_prepared_begin() {
    retained_admission_transfers_only_at_prepared_begin("session_bind");
}

#[test]
fn project_containment_retained_evaluate_transfers_only_at_prepared_begin() {
    retained_admission_transfers_only_at_prepared_begin("turn_evaluate");
}

#[tokio::test]
async fn human_prompt_ui_resume_preserves_retained_gated_admission_recovery() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    for lost in ["session_bind", "turn_evaluate"] {
        let (state, project, session, _root, receiver, transport) = parked_admission(&[lost]);
        let (paused_before, control_owned, interrupted_before, head_before, image_before) = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            eprintln!("OFF_PAUSE_BEFORE {}", json!({"lost": lost,
                "paused": record.orchestrator_auto_dispatch_blocked,
                "operatorPaused": record.engram.operator_paused,
                "capacityRetry": record.provider_capacity_retry.is_some(),
                "preview": record.session.preview,
                "controlOwned": control_pause_is_owned(record)}));
            (record.orchestrator_auto_dispatch_blocked, control_pause_is_owned(record),
                record.queued_prompts[0].engram_interrupted,
                serde_json::to_value(&record.queued_prompts[0]).unwrap(),
                EngramUnsettledControlImage {
                    operation: "unsettled_control_image".into(),
                    routing_token: record.engram.routing_token.clone(),
                    active_grant_id: record.engram.active_grant_id.clone(),
                    uncertain_grant_id: record.engram.uncertain_grant_id.clone(),
                    begins_recorded: record.engram.begins_recorded,
                    bind_in_progress: record.engram.bind_in_progress,
                    checkpoint_in_progress: record.engram.checkpoint_in_progress,
                    existing_checkpoint: record.engram.checkpoint_operation.clone(),
                    runtime_running: false,
                    parked_admission_retry: record.engram.admission_retry.clone(),
                    parked_bind_retry: record.engram.bind_retry.clone(),
                    queued_prompts: record.queued_prompts.iter().cloned().collect(),
                    begin_requested: record.engram.pending_dispatch.as_ref()
                        .and_then(|pending| pending.begin_requested.clone()),
                    dispatch_generation: record.engram.dispatch_generation,
                })
        };
        state.update_project_engram_settings(&project, settings(&state, &project, false)).unwrap();
        let original = transport
            .inner
            .requests()
            .into_iter()
            .find(|r| r.request["operation"] == lost)
            .unwrap()
            .request;
        let off_debt = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(ordinary_off_execution_applies(&inner, &session));
            eprintln!("OFF_PAUSE_AFTER {}", json!({"lost": lost,
                "paused": record.orchestrator_auto_dispatch_blocked,
                "preview": record.session.preview, "controlOwnedBefore": control_owned}));
            assert_eq!(record.orchestrator_auto_dispatch_blocked, paused_before && !control_owned,
                "OFF releases only a positively owned control pause");
            assert_eq!(record.queued_prompts[0].engram_interrupted, interrupted_before,
                "OFF preserves the prior interruption marker");
            assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), head_before);
            let entry = engram_session_obligation_locked(&inner, &session).expect("OFF retains the original uncertainty");
            assert_eq!(entry.basis, EngramSessionObligationBasis::ControlStateUnsettled);
            assert_eq!(entry.local_session_id, session);
            assert_eq!(entry.project_id, project);
            assert_eq!(entry.prompt_id.as_deref(), Some(record.queued_prompts[0].pending_prompt.id.as_str()));
            assert_eq!(entry.owner_generation, image_before.dispatch_generation + 1);
            assert_eq!(entry.owner_generation, record.engram.dispatch_generation);
            assert_eq!(entry.frozen_request, serde_json::to_value(&image_before).unwrap());
            let head = &image_before.queued_prompts[0];
            let request = if lost == "session_bind" { &head.engram_bind.as_ref().unwrap().request }
                else { &head.engram_evaluate.as_ref().unwrap().request };
            assert_eq!(serde_json::to_value(request).unwrap(), original);
            serde_json::to_value(&inner.engram_session_obligations).unwrap()
        };
        let off_requests = transport.inner.requests().len();
        state.resume_session_queue(&session).unwrap();
        {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(!record.orchestrator_auto_dispatch_blocked,
                "one ordinary Resume releases the unproven pause without replay");
            assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), head_before);
            assert_eq!(serde_json::to_value(&inner.engram_session_obligations).unwrap(), off_debt);
        }
        assert!(receiver.try_recv().is_err());
        assert_eq!(transport.inner.requests().len(), off_requests);
        let before_enable = transport.inner.requests().len();
        state
            .update_project_engram_settings(&project, settings(&state, &project, true))
            .unwrap();
        let selection = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(record.orchestrator_auto_dispatch_blocked);
            assert!(record.queued_prompts[0].engram_interrupted);
            assert_eq!(record.session.preview,
                "Engram recovered authorization retained. Resume to recover the exact queued prompt.",
                "ON re-enable preserves its exact Resume recovery policy");
            assert_eq!(transport.inner.requests().len(), before_enable,
                "enabling control restores a hold without replaying the operation");
            HumanPromptSelection {
                prompt: projected_pending_prompts(record).remove(0),
                session_mutation_stamp: record.mutation_stamp,
            }
        };
        let response = app_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{session}/queue/resume"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&ResumeQueueRequest {
                            origin: Some(ExplicitPromptOrigin::Human),
                            human_choice: Some(selection),
                        })
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "UI-selected {lost} recovery"
        );
        assert!(transport.transfer_seen.load(Ordering::SeqCst));
        assert!(matches!(
            receiver.try_recv().unwrap(),
            CodexRuntimeCommand::Prompt { .. }
        ));
        assert!(receiver.try_recv().is_err());
        let requests = transport.inner.requests();
        let replay = requests
            .iter()
            .filter(|r| r.request["operation"] == lost)
            .collect::<Vec<_>>();
        assert_eq!(replay.len(), 2);
        assert_eq!(replay[1].request, original);
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.request["operation"] == "turn_begin")
                .count(),
            1
        );
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.human_queue_choices.is_empty());
        assert!(
            record.human_prompt_turn.is_none(),
            "existing control owner remains gated"
        );
    }
}

fn retained_admission_transfers_only_at_prepared_begin(lost: &str) {
    let (state, project, session, _root, receiver, transport) = retained_admission(&[lost]);
    let off_requests = transport.inner.requests().len();
    state.resume_session_queue(&session).unwrap();
    assert_eq!(transport.inner.requests().len(), off_requests,
        "OFF Resume must not replay retained control");
    assert!(receiver.try_recv().is_err(),
        "an old promoted attempt is not an ordinary provider send");
    let original = transport
        .inner
        .requests()
        .iter()
        .find(|r| r.request["operation"] == lost)
        .unwrap()
        .request
        .clone();
    let count = transport.inner.requests().len();
    state
        .update_project_engram_settings(&project, settings(&state, &project, true))
        .unwrap();
    assert_eq!(
        transport.inner.requests().len(),
        count,
        "ON PATCH sends no control request"
    );
    {
        let inner = state.inner.lock().unwrap();
        assert!(engram_session_obligation_locked(&inner, &session).is_some());
        assert!(inner.engram_confirmed_containment_ids.is_empty());
    }
    state.resume_session_queue(&session).unwrap();
    assert!(transport.transfer_seen.load(Ordering::SeqCst));
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    assert!(receiver.try_recv().is_err());
    state.resume_session_queue(&session).unwrap();
    assert!(receiver.try_recv().is_err(), "one provider handoff only");
    let requests = transport.inner.requests();
    let replays = requests
        .iter()
        .filter(|r| r.request["operation"] == lost)
        .collect::<Vec<_>>();
    assert_eq!(replays.len(), 2);
    assert_eq!(replays[1].request, original);
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.request["operation"] == "turn_begin")
            .count(),
        1
    );
    let operations = requests
        .iter()
        .map(|r| r.request["operation"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        operations,
        if lost == "session_bind" {
            vec![
                "session_bind",
                "session_bind",
                "turn_evaluate",
                "turn_begin",
            ]
        } else {
            vec![
                "session_bind",
                "turn_evaluate",
                "session_status",
                "turn_evaluate",
                "turn_begin",
            ]
        }
    );
}

#[test]
fn project_containment_lost_begin_keeps_the_transferred_exact_recovery_owner() {
    let (state, project, session, _root, receiver, transport) =
        retained_admission(&["turn_evaluate", "turn_begin"]);
    state
        .update_project_engram_settings(&project, settings(&state, &project, true))
        .unwrap();
    let _ = state.resume_session_queue(&session);
    assert!(transport.transfer_seen.load(Ordering::SeqCst));
    assert!(receiver.try_recv().is_err());
    let before = transport.inner.requests();
    let original_begin = before
        .iter()
        .find(|r| r.request["operation"] == "turn_begin")
        .unwrap()
        .request
        .clone();
    state.resume_session_queue(&session).unwrap();
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    assert!(receiver.try_recv().is_err());
    let after = transport.inner.requests();
    let begins = after
        .iter()
        .filter(|r| r.request["operation"] == "turn_begin")
        .collect::<Vec<_>>();
    assert_eq!(begins.len(), 2);
    assert_eq!(begins[1].request, original_begin);
    assert_eq!(
        after
            .iter()
            .filter(|r| r.request["operation"] == "turn_evaluate")
            .count(),
        2,
        "lost Begin never creates a replacement Evaluate"
    );
    state.shutdown_persist_blocking();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(
        loaded
            .engram_confirmed_containment_ids
            .contains(&loaded.engram_session_obligations.entries[0].id)
    );
}

#[test]
fn project_containment_retained_recovery_refuses_changed_owner_payload_and_independent_duties() {
    for changed in [
        "owner",
        "payload",
        "key",
        "grant",
        "independent",
        "authority",
    ] {
        let (state, project, session, _root, receiver, transport) =
            retained_admission(&["turn_evaluate"]);
        state
            .update_project_engram_settings(&project, settings(&state, &project, true))
            .unwrap();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            match changed {
                "owner" => record.engram.dispatch_generation += 1,
                "payload" => record.queued_prompts[0]
                    .pending_prompt
                    .text
                    .push_str(" changed"),
                "key" => {
                    if let EngramControlRequest::TurnEvaluate {
                        idempotency_key, ..
                    } = &mut record.queued_prompts[0]
                        .engram_evaluate
                        .as_mut()
                        .unwrap()
                        .request
                    {
                        idempotency_key.push_str("-changed");
                    }
                }
                "grant" => record.engram.active_grant_id = Some("independent-begun-grant".into()),
                "authority" => {
                    inner
                        .projects
                        .iter_mut()
                        .find(|p| p.id == project)
                        .unwrap()
                        .engram
                        .as_mut()
                        .unwrap()
                        .authority_store_key
                        .as_mut()
                        .unwrap()
                        .project_id
                        .push_str("-changed");
                }
                "independent" => {
                    let mut independent = inner.engram_session_obligations.entries[0].clone();
                    independent.id.push_str("-independent");
                    independent.grant_id = "independent-grant".into();
                    independent.prompt_id = None;
                    let mut image: EngramUnsettledControlImage =
                        serde_json::from_value(independent.frozen_request).unwrap();
                    image.active_grant_id = Some(independent.grant_id.clone());
                    image.queued_prompts.clear();
                    independent.frozen_request = serde_json::to_value(image).unwrap();
                    inner.engram_session_obligations = inner
                        .engram_session_obligations
                        .with_entry(independent)
                        .unwrap();
                }
                _ => unreachable!(),
            }
            state.commit_locked(&mut inner).unwrap();
        }
        let count = transport.inner.requests().len();
        assert!(state.resume_session_queue(&session).is_err(), "{changed}");
        assert_eq!(transport.inner.requests().len(), count, "{changed}");
        assert!(receiver.try_recv().is_err());
        assert!(
            state
                .inner
                .lock()
                .unwrap()
                .engram_confirmed_containment_ids
                .is_empty()
        );
    }
}

#[test]
fn project_containment_resume_scope_never_authorizes_fresh_send_or_another_thread() {
    let (state, project, session, _root, receiver, _transport) =
        retained_admission(&["turn_evaluate"]);
    state
        .update_project_engram_settings(&project, settings(&state, &project, true))
        .unwrap();
    let (failures, interrupted) = {
        let inner = state.inner.lock().unwrap();
        assert!(!containment_retained_reconciliation_allowed(
            &inner, &session
        ));
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        (
            record.engram.consecutive_transport_failures,
            record.queued_prompts[0].engram_interrupted,
        )
    };
    assert!(
        failures > 0,
        "the bridge must preserve real transport failure history"
    );
    let scope = state
        .begin_contained_admission_resume(&session)
        .unwrap()
        .unwrap();
    {
        let inner = state.inner.lock().unwrap();
        assert!(engram_session_obligation_locked(&inner, &session).is_some());
        assert!(engram_admission_obligation_locked(&inner, &session).is_none());
        assert!(containment_retained_reconciliation_allowed(
            &inner, &session
        ));
    }
    state
        .release_engram_begin_preparation_hold_for_resume(&session, None)
        .unwrap();
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.engram.consecutive_transport_failures, failures);
        assert!(!record.engram.recovered_admission);
        assert_eq!(record.queued_prompts[0].engram_interrupted, interrupted);
    }
    assert!(
        state.dispatch_turn(&session, prompt()).is_err(),
        "fresh Send cannot consume a recovery permit"
    );
    assert!(
        state.resume_session_queue(&session).is_err(),
        "a reentrant call cannot inherit the outer Resume owner's permit"
    );
    assert!(
        state
            .prepare_dedicated_runtime_reset_off_lock(&session, true)
            .is_err(),
        "an unrelated model refresh remains excluded on the same thread"
    );
    std::thread::scope(|threads| {
        threads
            .spawn(|| {
                let inner = state.inner.lock().unwrap();
                assert!(engram_admission_obligation_locked(&inner, &session).is_some());
                assert!(!containment_retained_reconciliation_allowed(
                    &inner, &session
                ));
            })
            .join()
            .unwrap();
    });
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let original = inner.sessions[index].queued_prompts[0]
            .engram_evaluate
            .clone();
        if let EngramControlRequest::TurnEvaluate {
            idempotency_key, ..
        } = &mut inner.sessions[index].queued_prompts[0]
            .engram_evaluate
            .as_mut()
            .unwrap()
            .request
        {
            idempotency_key.push_str("-changed-inside-flight");
        }
        assert!(
            engram_admission_obligation_locked(&inner, &session).is_some(),
            "even an active recovery permit cannot replay a changed operation"
        );
        assert!(!containment_retained_reconciliation_allowed(
            &inner, &session
        ));
        inner.sessions[index].queued_prompts[0].engram_evaluate = original;
        inner.sessions[index].engram.uncertain_grant_id = Some("independent-unknown".into());
        assert!(!containment_retained_reconciliation_allowed(
            &inner, &session
        ));
        inner.sessions[index].engram.uncertain_grant_id = None;
        inner.sessions[index].runtime_stop_in_progress = true;
        assert!(!containment_retained_reconciliation_allowed(
            &inner, &session
        ));
        inner.sessions[index].runtime_stop_in_progress = false;
        assert!(containment_retained_reconciliation_allowed(
            &inner, &session
        ));
    }
    drop(scope);
    let inner = state.inner.lock().unwrap();
    assert!(engram_admission_obligation_locked(&inner, &session).is_some());
    assert!(!containment_retained_reconciliation_allowed(
        &inner, &session
    ));
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_reconciled_refuse_and_defer_keep_their_actual_decisions() {
    for case in ["refuse", "defer", "begun", "unknown"] {
        let defer = case == "defer";
        let independent = matches!(case, "begun" | "unknown");
        let (state, project, session, _root, receiver) = fixture();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].engram = EngramSessionState::default();
            inner.sessions[index].engram.context_nudge_pending = false;
            inner.sessions[index].session.status = SessionStatus::Idle;
            state.commit_locked(&mut inner).unwrap();
        }
        let decision = if defer {
            defer_reply("busy")
        } else {
            evaluation_refusal_reply("operator_review_required")
        };
        let status = if independent {
            ScriptedEngramControlResponse::Reply(Ok(json!({
                "phase": "ready", "open_grant_id": "independent-grant",
                "open_grant_state": case
            })))
        } else {
            status_reply("ready")
        };
        let transport = ScriptedEngramControlTransport::new([
            bind_reply("original-token"),
            ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
                "original decision reply lost",
            ))),
            status,
            decision,
        ]);
        state.install_control_test_transport(transport.clone());
        let dispatch = match state.dispatch_turn(&session, prompt()).unwrap() {
            DispatchTurnResult::Dispatched(d) | DispatchTurnResult::DispatchedAfterQueue(d) => d,
            DispatchTurnResult::Queued => panic!("initial admission must run"),
        };
        deliver_turn_dispatch(&state, dispatch).unwrap();
        assert!(receiver.try_recv().is_err());
        state
            .update_project_engram_settings(&project, settings(&state, &project, false))
            .unwrap();
        state
            .update_project_engram_settings(&project, settings(&state, &project, true))
            .unwrap();
        let _ = state.resume_session_queue(&session);
        assert!(receiver.try_recv().is_err());
        let requests = transport.requests();
        let evaluations = requests
            .iter()
            .filter(|r| r.request["operation"] == "turn_evaluate")
            .collect::<Vec<_>>();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.request["operation"] == "session_status")
                .count(),
            1
        );
        if independent {
            assert_eq!(
                evaluations.len(),
                1,
                "an independent producer duty stops replay"
            );
            assert!(!requests.iter().any(|request| matches!(
                request.request["operation"].as_str(),
                Some("turn_checkpoint" | "turn_begin")
            )));
            let inner = state.inner.lock().unwrap();
            assert!(engram_session_obligation_locked(&inner, &session).is_some());
            assert!(inner.engram_confirmed_containment_ids.is_empty());
            continue;
        }
        assert_eq!(evaluations.len(), 2);
        assert_eq!(evaluations[0].request, evaluations[1].request);
        assert!(
            !requests
                .iter()
                .any(|r| r.request["operation"] == "turn_begin")
        );
        let inner = state.inner.lock().unwrap();
        let entry = &inner.engram_session_obligations.entries[0];
        assert!(containment_admission_decision_matches(entry));
        assert!(inner.engram_confirmed_containment_ids.contains(&entry.id));
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        let expected_decision = if defer {
            EngramControlCardDecision::Defer
        } else {
            EngramControlCardDecision::Refuse
        };
        assert!(record.session.messages.iter().any(|m| matches!(m,
            Message::EngramControl { card, .. }
                if card.decision == expected_decision
                    && card.grant_id.is_none() && card.dispatch == EngramControlCardDispatch::Withheld)));
        if defer {
            assert!(record.queued_prompts[0].engram_waiting);
            assert!(record.queued_prompts[0].engram_evaluate.is_none());
            assert!(record.orchestrator_auto_dispatch_blocked);
        }
    }
}

#[test]
fn project_containment_admission_transfer_ack_is_required_and_covers_the_successor() {
    let (state, project, session, root, receiver, transport) =
        retained_admission(&["turn_evaluate"]);
    state
        .update_project_engram_settings(&project, settings(&state, &project, true))
        .unwrap();
    let scope = state
        .begin_contained_admission_resume(&session)
        .unwrap()
        .unwrap();
    let original_owner = {
        let inner = state.inner.lock().unwrap();
        EngramQueuedAdmissionOwner::capture(
            &inner.sessions[inner.find_session_index(&session).unwrap()],
        )
        .unwrap()
    };
    // Hold the real Resume promotion undelivered at the transfer ACK boundary.
    let _dispatch = state
        .dispatch_next_queued_turn_for_resume(&session, original_owner.clone())
        .unwrap()
        .expect("the retained admission must promote before preparing Begin");
    let grant = transport.inner.grant_state(&session).0.unwrap();
    let (owner, begin) = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        assert_eq!(
            Some(record.active_turn_generation),
            original_owner.active_turn_generation.checked_add(1)
        );
        let evaluate = record.queued_prompts[0].engram_evaluate.as_mut().unwrap();
        assert_eq!(
            Some(record.engram.dispatch_generation),
            evaluate.operation_generation
        );
        let routing_token = match &evaluate.request {
            EngramControlRequest::TurnEvaluate { routing_token, .. } => routing_token.clone(),
            _ => unreachable!(),
        };
        let begin = EngramControlRequest::TurnBegin {
            routing_token,
            grant_id: grant.clone(),
            delivery_tokens: Vec::new(),
            idempotency_key: "exact-transfer-begin".into(),
        };
        (EngramQueuedAdmissionOwner::capture(record).unwrap(), begin)
    };
    let count = transport.inner.requests().len();
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(false));
    let result = state.prepare_engram_begin_durable(
        &session,
        state.engram_budget_clock().now() + Duration::from_secs(5),
        &owner,
        &begin,
        None,
        EngramBeginPreparationKind::First,
    );
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(true));
    let failure = result
        .err()
        .expect("a missing transfer acknowledgement withholds Begin");
    assert!(
        failure.pending,
        "the exact successor is kept before any Begin transmission"
    );
    assert!(!failure.possibly_sent);
    drop(scope);
    {
        let inner = state.inner.lock().unwrap();
        assert!(inner.engram_confirmed_containment_ids.is_empty());
        assert!(engram_session_obligation_locked(&inner, &session).is_some());
        let entry = &inner.engram_session_obligations.entries[0];
        assert!(containment_admission_transfer_matches(entry));
        let image =
            EngramProjectContainmentFence::capture(&inner, &project, &[session.clone()]).unwrap();
        let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
        assert!(image.is_already_durable(&connection).unwrap());
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        let original: String = connection
            .query_row(
                "SELECT value_json FROM sessions WHERE id = ?1",
                [&session],
                |r| r.get(0),
            )
            .unwrap();
        let mut changed: Value = serde_json::from_str(&original).unwrap();
        changed["queuedPrompts"][0]["engram_evaluate"]["prepared_begin"]["idempotency_key"] =
            json!("different-begin");
        connection
            .execute(
                "UPDATE sessions SET value_json = ?1 WHERE id = ?2",
                rusqlite::params![changed.to_string(), session],
            )
            .unwrap();
        assert!(
            !image.is_already_durable(&connection).unwrap(),
            "metadata alone cannot acknowledge a different successor"
        );
        connection.execute_batch("ROLLBACK").unwrap();
    }
    state
        .retry_containment_admission_transfer(&session)
        .unwrap();
    let failing = AppState {
        persistence_path: Arc::new(root),
        ..state.clone()
    };
    let failure = failing
        .prepare_engram_begin_durable(
            &session,
            state.engram_budget_clock().now() + Duration::from_secs(5),
            &owner,
            &begin,
            None,
            EngramBeginPreparationKind::Pending,
        )
        .err()
        .expect("directory path refuses the later preparation write");
    assert!(
        failure.pending,
        "a failed later fence cannot withdraw the transferred successor"
    );
    assert!(!failure.possibly_sent);
    let inner = state.inner.lock().unwrap();
    assert!(engram_session_obligation_locked(&inner, &session).is_none());
    assert!(
        inner.sessions[inner.find_session_index(&session).unwrap()].queued_prompts[0]
            .engram_evaluate
            .as_ref()
            .unwrap()
            .prepared_begin
            .is_some()
    );
    assert_eq!(
        transport.inner.requests().len(),
        count,
        "acknowledgement retry sends no control request"
    );
    assert!(receiver.try_recv().is_err());
}
