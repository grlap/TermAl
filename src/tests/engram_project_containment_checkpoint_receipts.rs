//! Ordinary exact selection and delivery fences preserve passive history.
//! Historical checkpoint producers and their accounting are physically gone.
use super::*;

fn ordinary_receipt_fixture(queued: bool, receipted: bool) -> (
    AppState, String, String, Arc<ScriptedEngramControlTransport>,
    mpsc::Receiver<CodexRuntimeCommand>,
) {
    let (state, receiver) = test_app_state_with_delegation_codex_runtime("ordinary-receipt-runtime");
    let root=state.test_temp_root.as_ref().unwrap().path().join("ordinary-receipt-project");
    fs::create_dir_all(&root).unwrap();
    let project=create_test_project(&state,&root,"Ordinary receipt");
    let session=create_test_project_session(&state,Agent::Codex,&project,&root);
    enable_test_project_engram(&state,&project,&root);
    let transport=ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(transport.clone());
    {
        let mut inner=state.inner.lock().unwrap();
        let record=inner.session_mut(&session).unwrap();
        record.engram.historical_control.insert("engramAdmissionRetry".into(), json!({
            "originalSession":session,"requestKey":"original-key",
            "receipt":{"extension":[1,2,3],"exact":"\u{1}".repeat(32*1024)},
        }));
        record.engram.historical_control.insert("engramOpenGrantId".into(),json!("historical-grant"));
        record.set_auto_dispatch_blocked(true);
        if queued {
            let pending=PendingPrompt { id:"queued-before-off".into(), timestamp:stamp_now(),
                text:"ordinary queued input".into(), expanded_text:None, source:None,
                attachments:Vec::new(), engram_interrupted:false, is_engram_retained:false };
            if receipted {
                queue_receipted_orchestrator_prompt_on_record(record,pending,Vec::new());
                record.queued_prompts[0].source=QueuedPromptSource::User;
                record.queued_prompts[0].engram_interrupted=true;
                record.queued_prompt_durability_holds.get_mut("queued-before-off").unwrap().content=
                    queued_prompt_durability_content(&record.queued_prompts[0]);
            } else { queue_prompt_on_record(record,pending,Vec::new()); }
        }
        state.commit_locked(&mut inner).unwrap();
    }
    assert!(transport.requests().is_empty());
    (state,project,session,transport,receiver)
}

fn history(state:&AppState,session:&str)->Value {
    let inner=state.inner.lock().unwrap();
    serde_json::to_value(&inner.sessions[inner.find_session_index(session).unwrap()].engram.historical_control).unwrap()
}

fn assert_history_reloads(state:&AppState,session:&str,expected:&Value) {
    assert_eq!(&history(state,session),expected);
    let loaded=load_state(state.persistence_path.as_path()).unwrap().unwrap();
    let record=&loaded.sessions[loaded.find_session_index(session).unwrap()];
    assert_eq!(&serde_json::to_value(&record.engram.historical_control).unwrap(),expected);
}

struct ReceiptWriter {
    tx:mpsc::Sender<PersistRequest>,
    held:mpsc::Receiver<Box<PersistFence>>,
    thread:Option<std::thread::JoinHandle<()>>,
}
impl ReceiptWriter {
    fn attach(state:&mut AppState)->Self {
        state.shutdown_persist_blocking();
        let (tx,rx)=mpsc::channel();
        let (held_tx,held)=mpsc::channel();
        state.persist_tx=tx.clone();
        let inner=state.inner.clone();
        let path=state.persistence_path.clone();
        let thread=std::thread::spawn(move|| {
            let mut cache=SqlitePersistConnectionCache::new();
            let mut fences=PersistFenceBatch::default();
            while let Ok(request)=rx.recv() {
                if matches!(request,PersistRequest::Shutdown) { break; }
                if let PersistRequest::Fence(fence)=request {
                    if matches!(&fence.target,PersistFenceTarget::PromptDeliveryWriteAhead{..}) {
                        let delta=collect_persist_delta_from_shared_state(&inner,0);
                        persist_delta_via_cache(&mut cache,path.as_path(),&delta).unwrap();
                        let connection=cache.connection_for(path.as_path()).unwrap();
                        assert!(fence.target.is_already_durable(connection).unwrap());
                        held_tx.send(fence).unwrap();
                        continue;
                    }
                    fences.accept(PersistRequest::Fence(fence));
                }
                let delta=collect_persist_delta_from_shared_state(&inner,0);
                persist_delta_with_fences(&mut cache,path.as_path(),&delta,&mut fences).unwrap();
            }
            fences.fail(PersistFenceError::WorkerStopped);
        });
        Self{tx,held,thread:Some(thread)}
    }
    fn write_ahead(&self)->Box<PersistFence> {
        phase_sync::receive(&self.held,"the exact ordinary write-ahead")
    }
}
impl Drop for ReceiptWriter {
    fn drop(&mut self) {
        let _=self.tx.send(PersistRequest::Shutdown);
        self.thread.take().unwrap().join().expect("SQLite receipt writer finishes");
    }
}

#[tokio::test]
async fn human_prompt_ui_selected_ordinary_resume_dispatches_preserving_stored_history() {
    use axum::{body::Body, http::{Request, StatusCode}};
    use tower::ServiceExt;
    let (state, _project, session, transport, receiver) = ordinary_receipt_fixture(true,false);
    let original = history(&state,&session);
    let selected = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        HumanPromptSelection { prompt: projected_pending_prompts(record).remove(0),
            session_mutation_stamp: record.mutation_stamp }
    };
    let response = app_router(state.clone()).oneshot(Request::builder().method("POST")
        .uri(format!("/api/sessions/{session}/queue/resume"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&ResumeQueueRequest {
            origin: Some(ExplicitPromptOrigin::Human), human_choice: Some(selected),
        }).unwrap())).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(matches!(phase_sync::receive(&receiver, "OFF selected ordinary prompt"),
        CodexRuntimeCommand::Prompt { command, .. } if command.prompt.contains("ordinary queued input")));
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    assert_eq!(serde_json::to_value(&inner.sessions[inner.find_session_index(&session).unwrap()].engram.historical_control).unwrap(), original);

    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(record.queued_prompts.is_empty());
    assert!(record.human_prompt_turn.is_none(), "the original ordinary source stays ordinary");
    assert!(record.human_queue_choices.is_empty());
    assert!(transport.requests().is_empty());
    drop(inner);
    assert_history_reloads(&state,&session,&original);
}

#[test]
fn human_prompt_ordinary_selected_unknown_head_refuses_then_plain_resume_runs_successor() {
    let (state, _project, session, transport, receiver) = ordinary_receipt_fixture(true,false);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let head = &mut inner.sessions[index].queued_prompts[0];
        head.promotion_disposition_known = false;
        head.engram_interrupted = true;
    }
    queue_test_engram_prompt(&state,&session,"independent Human successor",QueuedPromptSource::Human,None);
    let (old_head, original) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        (serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            serde_json::to_value(&inner.sessions[inner.find_session_index(&session).unwrap()].engram.historical_control).unwrap())
    };
    let selection = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        HumanPromptSelection { prompt: projected_pending_prompts(record).remove(0),
            session_mutation_stamp: record.mutation_stamp }
    };
    let before_selection = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        serde_json::to_value(&record.queued_prompts).unwrap()
    };
    let refused = state.resume_selected_human_prompt(&session, selection)
        .err().expect("an unknown selected head must be refused");
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert!(receiver.try_recv().is_err(), "selecting unknown A never authorizes successor B");
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(serde_json::to_value(&record.queued_prompts).unwrap(), before_selection);
        assert_eq!(serde_json::to_value(&inner.sessions[inner.find_session_index(&session).unwrap()].engram.historical_control).unwrap(), original);

        assert!(record.human_queue_choices.is_empty());
    }
    // A bodyless OFF Resume is a separate ordinary activation, bound by its
    // own runnable queue owner rather than authority borrowed from unknown A.
    state.resume_session_queue(&session).unwrap();
    assert!(matches!(phase_sync::receive(&receiver, "independent OFF Human successor"),
        CodexRuntimeCommand::Prompt { command, .. } if command.prompt.contains("independent Human successor")
            && !command.prompt.contains("ordinary queued input")));
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    assert_eq!(serde_json::to_value(&inner.sessions[inner.find_session_index(&session).unwrap()].engram.historical_control).unwrap(), original);

    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), old_head);
    assert!(record.human_queue_choices.is_empty());
    assert!(record.human_prompt_turn.is_some());
    assert!(transport.requests().is_empty());
    drop(inner);
    assert_history_reloads(&state,&session,&original);
}

#[test]
fn human_prompt_reusable_selected_ordinary_resume_runs_its_wal_preserving_stored_history() {
    let (mut state, _project, session, transport, receiver) = ordinary_receipt_fixture(true,true);
    let writer = ReceiptWriter::attach(&mut state);
    let (content_nonce, content) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        let hold = &record.queued_prompt_durability_holds["queued-before-off"];
        assert!(!hold.acknowledged);
        (hold.nonce.clone(), hold.content.clone())
    };
    assert!(state.request_queued_prompt_durability(&session, "queued-before-off"));
    let guard = phase_sync::PollGuard::new();
    loop {
        let acknowledged = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            let hold = &record.queued_prompt_durability_holds["queued-before-off"];
            assert_eq!(hold.nonce, content_nonce);
            assert_eq!(hold.content, content);
            hold.acknowledged
        };
        if acknowledged { break; }
        guard.wait("actual content durability acknowledgement");
    }
    assert!(receiver.try_recv().is_err(), "content ACK alone cannot resume a paused queue");
    let (selection, original) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        let receipt = &record.prompt_deliveries["queued-before-off"];
        assert!(receipt.federation_operation.is_none());
        assert_eq!(receipt.disposition, PromptDeliveryDisposition::Pending);
        assert!(receipt.turn_generation.is_none());
        assert!(!receipt.write_ahead_acknowledged,
            "content durability does not acknowledge the separate handoff WAL");
        assert!(record.queued_prompt_durability_holds["queued-before-off"].acknowledged);
        (HumanPromptSelection { prompt: projected_pending_prompts(record).remove(0),
            session_mutation_stamp: record.mutation_stamp },
            serde_json::to_value(&inner.sessions[inner.find_session_index(&session).unwrap()].engram.historical_control).unwrap())
    };
    state.resume_selected_human_prompt(&session, selection).unwrap();
    let wal_ack = writer.write_ahead();
    assert!(receiver.try_recv().is_err(), "provider handoff waits for the separate WAL acknowledgement");
    wal_ack.finish(Ok(()));
    assert!(matches!(phase_sync::receive(&receiver, "OFF exact receipted ordinary prompt"),
        CodexRuntimeCommand::Prompt { command, .. } if command.prompt.contains("ordinary queued input")));
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    assert_eq!(serde_json::to_value(&inner.sessions[inner.find_session_index(&session).unwrap()].engram.historical_control).unwrap(), original);

    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.queued_prompts.len(), 1);
    let retained = &record.queued_prompts[0];
    assert_eq!(retained.pending_prompt.id, "queued-before-off");
    assert!(retained.engram_ordinary_handoff);
    let promoted = retained.promoted_message_index.expect("ordinary handoff has a transcript owner");
    assert_eq!(Some(promoted), record.message_positions.get("queued-before-off")
        .map(|position| record.message_start_index + position));
    let mut expected_row = content.clone();
    expected_row["engram_ordinary_handoff"] = json!(true);
    expected_row["promoted_message_index"] = json!(promoted);
    assert_eq!(serde_json::to_value(retained).unwrap(), expected_row,
        "only this prompt's ordinary handoff markers may change");
    assert!(ordinary_queue_position(record).is_none(), "delivered history cannot be selected again");
    assert!(record.human_prompt_turn.is_none());
    assert!(record.human_queue_choices.is_empty());
    let receipt = &record.prompt_deliveries["queued-before-off"];
    assert_eq!(receipt.disposition, PromptDeliveryDisposition::HandingOff,
        "the channel proves handoff, not a provider turn/start acknowledgement");
    assert!(receipt.write_ahead_acknowledged);
    assert_eq!(receipt.turn_generation, Some(record.active_turn_generation));
    assert!(receipt.resume.is_none());
    assert!(transport.requests().is_empty());
    let token = record.runtime.runtime_token().unwrap();
    let generation = record.active_turn_generation;
    drop(inner);
    state.finish_turn_ok_if_runtime_and_generation_match(&session, &token, generation).unwrap();
    state.resume_session_queue(&session).unwrap();
    assert!(receiver.try_recv().is_err(), "completion and Resume cannot replay delivered history");
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), expected_row);
    let receipt = &record.prompt_deliveries["queued-before-off"];
    assert_eq!(receipt.disposition, PromptDeliveryDisposition::Accepted,
        "the matching runtime turn completion proves provider acceptance");
    assert!(receipt.write_ahead_acknowledged);
    assert_eq!(receipt.turn_generation, Some(generation));
    assert!(receipt.resume.is_none());
    assert!(ordinary_queue_position(record).is_none());
    assert_eq!(serde_json::to_value(&inner.sessions[inner.find_session_index(&session).unwrap()].engram.historical_control).unwrap(), original);

    assert!(transport.requests().is_empty());
    drop(inner);
    drop(writer);
    assert_history_reloads(&state,&session,&original);
}


#[test]
fn ordinary_completion_preserves_complete_historical_receipt_data() {
    let (state,_project,session,transport,receiver)=ordinary_receipt_fixture(false,false);
    let original=history(&state,&session);
    let dispatch=match state.dispatch_human_turn(&session,SendMessageRequest {
        text:"independent ordinary input".into(),expanded_text:None,attachments:Vec::new(),
        source_session_id:None,source_mailbox:None,
    }).unwrap() {
        DispatchTurnResult::Dispatched(d)|DispatchTurnResult::DispatchedAfterQueue(d)=>d,
        DispatchTurnResult::Queued=>panic!("history cannot block a new Human turn"),
    };
    let token=dispatch.runtime_token().clone();
    let generation=dispatch.active_turn_generation();
    assert!(matches!(deliver_turn_dispatch(&state,dispatch),TurnDispatchDeliveryOutcome::Delivered));
    assert!(matches!(phase_sync::receive(&receiver,"independent ordinary input"),
        CodexRuntimeCommand::Prompt{command,..} if command.prompt.contains("independent ordinary input")));
    state.finish_turn_ok_if_runtime_and_generation_match(&session,&token,generation).unwrap();
    assert!(receiver.try_recv().is_err());
    assert_history_reloads(&state,&session,&original);
    assert!(transport.requests().is_empty());
}

#[test]
fn ordinary_stale_selection_never_sends_replacement_content() {
    for changed in ["stamp","content","id"] {
        let (state,_project,session,transport,receiver)=ordinary_receipt_fixture(true,false);
        let selected={
            let inner=state.inner.lock().unwrap();
            let record=&inner.sessions[inner.find_session_index(&session).unwrap()];
            HumanPromptSelection{prompt:projected_pending_prompts(record)[0].clone(),
                session_mutation_stamp:record.mutation_stamp}
        };
        {
            let mut inner=state.inner.lock().unwrap();
            let record=inner.session_mut(&session).unwrap();
            match changed {
                "content"=>record.queued_prompts[0].pending_prompt.text="replacement".into(),
                "id"=>record.queued_prompts[0].pending_prompt.id="replacement-id".into(),
                _=>{},
            }
            sync_pending_prompts(record);
            state.commit_locked(&mut inner).unwrap();
        }
        let before={
            let inner=state.inner.lock().unwrap();
            serde_json::to_value(&inner.sessions[inner.find_session_index(&session).unwrap()].queued_prompts).unwrap()
        };
        let error=state.resume_selected_human_prompt(&session,selected).err().unwrap();
        assert_eq!(error.status,StatusCode::CONFLICT,"{changed}");
        assert!(receiver.try_recv().is_err());
        let inner=state.inner.lock().unwrap();
        let record=&inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(serde_json::to_value(&record.queued_prompts).unwrap(),before);
        assert!(record.human_queue_choices.is_empty());
        assert!(transport.requests().is_empty());
    }
}
