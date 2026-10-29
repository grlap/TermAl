// Owns exact terminal evidence, persistence and recovery-exclusion controls.
// Does not access live authority or claim a scripted reply proves remote state.
use super::*;

pub(super) fn grant_read(session: &str, grant: &str, state: &str) -> ScriptedEngramControlResponse {
    ScriptedEngramControlResponse::Reply(Ok(json!({
        "control_schema_version": ENGRAM_CONTROL_SCHEMA_VERSION,
        "session_id": session, "grant_id": grant, "status": "found", "state": state,
        "begun_at": if matches!(state, "begun" | "completed") { Some("2026-10-23T00:00:00Z") } else { None },
        "completed_at": if state == "completed" { Some("2026-10-23T00:01:00Z") } else { None },
    })))
}

#[test]
fn project_containment_terminal_cleanup_requires_exact_original_evidence() {
    for terminal in ["completed", "expired", "superseded"] {
        let (state, project, session, _root, receiver) = fixture();
        let transport = ScriptedEngramControlTransport::new([
            checkpoint_refusal_reply("GrantScopeMismatch"),
            grant_read(&session, "retained-grant", terminal),
        ]);
        install_control_only_transport(&state, transport.clone());
        let response = state
            .update_project_engram_settings(&project, settings(&state, &project, false))
            .unwrap();
        assert!(response.engram_control_recovery.is_empty());
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.engram.active_grant_id.is_none());
        assert!(
            inner
                .engram_session_obligations
                .entries
                .iter()
                .all(
                    |entry| entry.phase == EngramSessionObligationPhase::Confirmed
                        && !engram_obligation_requires_exclusion(&inner, entry)
                )
        );
        let requests = transport.requests();
        let read = requests
            .iter()
            .find(|request| request.request["operation"] == "turn_grant_read")
            .unwrap();
        assert_eq!(read.connection.session_id, session);
        assert_eq!(read.request["grant_id"], "retained-grant");
        assert_eq!(read.request["routing_token"], "retained-token");
        assert!(
            !requests
                .iter()
                .any(|request| request.request["operation"] == "session_bind")
        );
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn project_containment_nonterminal_absent_or_wrong_identity_never_clears_grant() {
    for read_case in [
        "issued",
        "begun",
        "not_found",
        "wrong_owner",
        "successor",
        "schema",
        "malformed_time",
    ] {
        let (state, project, session, _root, _receiver) = fixture();
        let mut reply = match grant_read(&session, "retained-grant", "completed") {
            ScriptedEngramControlResponse::Reply(Ok(reply)) => reply,
            _ => unreachable!(),
        };
        match read_case {
            "issued" => {
                reply["state"] = json!("issued");
                reply["begun_at"] = Value::Null;
                reply["completed_at"] = Value::Null;
            }
            "begun" => {
                reply["state"] = json!("begun");
                reply["completed_at"] = Value::Null;
            }
            "not_found" => {
                reply = json!({"control_schema_version": 1, "session_id": session,
                "grant_id": "retained-grant", "status": "not_found"});
            }
            "wrong_owner" => reply["session_id"] = json!("another-owner"),
            "successor" => reply["grant_id"] = json!("successor-grant"),
            "schema" => reply["control_schema_version"] = json!(99),
            "malformed_time" => reply["begun_at"] = json!("not-a-timestamp"),
            _ => unreachable!(),
        }
        install_control_only_transport(
            &state,
            ScriptedEngramControlTransport::new([
                checkpoint_refusal_reply("GrantScopeMismatch"),
                ScriptedEngramControlResponse::Reply(Ok(reply)),
            ]),
        );
        state
            .update_project_engram_settings(&project, settings(&state, &project, false))
            .unwrap();
        assert_retained(&state, &project, &session, "GrantScopeMismatch");
        let inner = state.inner.lock().unwrap();
        assert!(require_no_engram_session_obligation(&inner, &session).is_err(), "{read_case}");
        assert!(require_no_engram_local_dispatch_obligation(&inner, &session).is_ok(),
            "{read_case}: an unusable read does not settle debt or veto fresh unmediated OFF work");
    }
}

#[test]
fn project_containment_unknown_begin_remains_excluded_and_can_only_read_exact_grant() {
    let (state, project, session, _root, receiver) = fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.engram.active_grant_id = None;
        record.engram.uncertain_grant_id = Some("possibly-sent-grant".to_owned());
        state.commit_locked(&mut inner).unwrap();
    }
    let transport =
        ScriptedEngramControlTransport::new([grant_read(&session, "possibly-sent-grant", "begun")]);
    install_control_only_transport(&state, transport.clone());
    let response = state
        .update_project_engram_settings(&project, settings(&state, &project, false))
        .unwrap();
    assert_eq!(response.engram_control_recovery.len(), 1);
    assert!(response.engram_control_recovery[0].excluded);
    assert!(!response.engram_control_recovery[0].settlement_confirmed);
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].request["operation"], "turn_grant_read");
    assert_eq!(requests[0].request["grant_id"], "possibly-sent-grant");
    assert!(state.resume_local_session_queue(&session, None).is_err());
    assert!(state.dispatch_turn(&session, prompt()).is_err());
    assert!(receiver.try_recv().is_err());
    state.shutdown_persist_blocking();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&session).unwrap()];
    assert_eq!(
        record.engram.uncertain_grant_id.as_deref(),
        Some("possibly-sent-grant")
    );
    assert!(require_no_engram_session_obligation(&loaded, &session).is_err());
    assert_eq!(loaded.engram_session_obligations.entries.len(), 1);
    *state.inner.lock().unwrap() = loaded;
    let boot = state.prepare_engram_sessions_for_boot_recovery().unwrap();
    assert!(
        boot.targets.is_empty(),
        "boot cannot bootstrap the excluded control session"
    );
    assert!(state.resume_local_session_queue(&session, None).is_err());
    assert!(state.dispatch_turn(&session, prompt()).is_err());
    assert_eq!(
        transport.requests().len(),
        1,
        "restart and Resume send no recovery or bind RPC"
    );
}

#[test]
fn project_containment_first_reader_preserves_unhandled_retirement_and_its_owner_head() {
    let (state, project, session, root, receiver) = fixture();
    let mut entry = obligations::entry(&root);
    entry.project_id = project.clone();
    entry.local_session_id = session.clone();
    entry.connection.session_id = session.clone();
    entry.external_ref = format!("termal:session:{session}");
    entry.kind = EngramSessionObligationKind::IssuedGrantRetirement;
    entry.basis = EngramSessionObligationBasis::ProvenNeverSentBegin;
    entry.prompt_id = Some("original-owner-head".to_owned());
    entry.frozen_request = serde_json::to_value(EngramControlRequest::SessionBind {
        external_ref: entry.external_ref.clone(),
        title: "Retained retirement".to_owned(),
        assurance: ENGRAM_CONTROL_ASSURANCE.to_owned(),
        mediated_effects: Vec::new(),
        capability_map_revision: 1,
        work_binding: None,
        idempotency_key: "distinct-original-retirement-key".to_owned(),
    })
    .unwrap();
    let original = serde_json::to_value(&entry).unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.engram.active_grant_id = None;
        record.engram.routing_token = Some(entry.routing_token.clone());
        record.engram.dispatch_generation = entry.owner_generation;
        inner.engram_session_obligations = EngramSessionObligationLedger::empty()
            .with_entry(entry)
            .unwrap();
        state.commit_locked(&mut inner).unwrap();
    }
    let transport = ScriptedEngramControlTransport::new([]);
    install_control_only_transport(&state, transport.clone());
    let response = state
        .update_project_engram_settings(&project, settings(&state, &project, false))
        .unwrap();
    assert_eq!(
        response.engram_control_recovery[0].operation,
        "session_bind_retirement"
    );
    assert!(response.engram_control_recovery[0].excluded);
    assert!(state.dispatch_turn(&session, prompt()).is_err());
    assert!(state.resume_local_session_queue(&session, None).is_err());
    assert!(
        state
            .prepare_engram_sessions_for_boot_recovery()
            .unwrap()
            .targets
            .is_empty()
    );
    assert!(transport.requests().is_empty());
    assert!(receiver.try_recv().is_err());
    state.shutdown_persist_blocking();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&loaded.engram_session_obligations.entries[0]).unwrap(),
        original
    );
    assert!(loaded.engram_confirmed_containment_ids.is_empty());
    assert!(require_no_engram_session_obligation(&loaded, &session).is_err());
}

#[test]
fn project_containment_public_recovery_never_serializes_private_identity() {
    let (state, project, session, _root, _receiver) = fixture();
    let transport =
        ScriptedEngramControlTransport::new([checkpoint_refusal_reply("GrantNotBegun")]);
    install_control_only_transport(&state, transport.clone());
    let response = state
        .update_project_engram_settings(&project, settings(&state, &project, false))
        .unwrap();
    let view = serde_json::to_value(&response.engram_control_recovery).unwrap();
    assert_eq!(view[0]["projectId"], project);
    assert_eq!(view[0]["sessionId"], session);
    assert_eq!(view[0]["code"], "GrantNotBegun");
    assert_eq!(view[0]["excluded"], true);
    let text = view.to_string();
    for private in [
        "retained-token",
        "retained-grant",
        "frozenRequest",
        "connection",
        "databasePath",
    ] {
        assert!(
            !text.contains(private),
            "private recovery field escaped: {private}"
        );
    }
    let original = serde_json::to_value(
        &state
            .inner
            .lock()
            .unwrap()
            .engram_session_obligations
            .entries,
    )
    .unwrap();
    let requests = transport.requests();
    state
        .update_project_engram_settings(&project, settings(&state, &project, true))
        .unwrap();
    {
        let inner = state.inner.lock().unwrap();
        assert!(
            inner
                .find_project(&project)
                .unwrap()
                .engram
                .as_ref()
                .unwrap()
                .turn_gated_control
        );
        assert_eq!(
            serde_json::to_value(&inner.engram_session_obligations.entries).unwrap(),
            original
        );
        assert!(engram_session_obligation_locked(&inner, &session).is_some());
    }
    let after = transport.requests();
    assert_eq!(after.len(), requests.len());
    for (before, after) in requests.iter().zip(&after) {
        assert_eq!(before.connection, after.connection);
        assert_eq!(before.request, after.request);
    }
    state.delete_project(&project).unwrap();
    state.kill_session(&session).unwrap();
    let inner = state.inner.lock().unwrap();
    assert!(inner.find_project(&project).is_none());
    assert!(inner.find_session_index(&session).is_none());
    assert_eq!(serde_json::to_value(&inner.engram_session_obligations.entries).unwrap(), original);
    assert!(require_no_engram_admission_obligation(&inner, &session).is_err());
    assert_eq!(transport.requests().len(), requests.len());
}

#[test]
fn project_containment_precommit_persistence_failure_keeps_old_setting_and_sends_nothing() {
    let (state, project, _session, _root, receiver) = fixture();
    state.shutdown_persist_blocking();
    let path = state.persistence_path.as_path();
    let connection = rusqlite::Connection::open(path).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER refuse_containment BEFORE UPDATE ON state_metadata_units
        BEGIN SELECT RAISE(ABORT, 'injected containment metadata failure'); END;",
        )
        .unwrap();
    drop(connection);
    let transport = ScriptedEngramControlTransport::new([checkpoint_reply("retained-grant")]);
    install_control_only_transport(&state, transport.clone());
    assert!(
        state
            .update_project_engram_settings(&project, settings(&state, &project, false))
            .is_err()
    );
    let inner = state.inner.lock().unwrap();
    assert!(
        inner
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap()
            .turn_gated_control
    );
    assert!(inner.engram_session_obligations.entries.is_empty());
    assert!(!inner.engram_project_resets.contains(&project));
    assert!(transport.requests().is_empty());
    assert!(receiver.try_recv().is_err());
}

struct ChangingContainmentOwner {
    inner: std::sync::Weak<StateMutex<StateInner>>,
    session_id: String,
}

impl EngramControlTransport for ChangingContainmentOwner {
    fn request(
        &self,
        _connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        _timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        match request {
            EngramControlRequest::TurnCheckpoint { grant_id, .. } => {
                let shared = self.inner.upgrade().unwrap();
                let mut inner = shared.lock().unwrap();
                let index = inner.find_session_index(&self.session_id).unwrap();
                let record = inner.session_mut_by_index(index).unwrap();
                record.engram.dispatch_generation += 1;
                record.engram.routing_token = Some("successor-token".to_owned());
                record.engram.active_grant_id = Some("successor-grant".to_owned());
                Ok(json!({"decision":"checkpointed", "receipt":{
                    "grant_id": grant_id, "cursor":1, "confirmed_cursor":1,
                }}))
            }
            _ => Err(EngramTransportError::protocol(
                "unexpected owner-race fixture request",
            )),
        }
    }

    fn shutdown_session(&self, _session_id: &str) {}
}

#[test]
fn project_containment_successor_owner_survives_an_old_checkpoint_receipt() {
    let (state, project, session, _root, receiver) = fixture();
    install_control_only_transport(
        &state,
        Arc::new(ChangingContainmentOwner {
            inner: Arc::downgrade(&state.inner),
            session_id: session.clone(),
        }),
    );
    let response = state
        .update_project_engram_settings(&project, settings(&state, &project, false))
        .unwrap();
    assert_eq!(
        response.engram_control_recovery[0].code,
        "checkpoint_receipt_accounting_pending"
    );
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(
        record.engram.active_grant_id.as_deref(),
        Some("successor-grant")
    );
    assert_eq!(
        record.engram.routing_token.as_deref(),
        Some("successor-token")
    );
    assert_eq!(
        inner.engram_session_obligations.entries[0].grant_id,
        "retained-grant"
    );
    assert_eq!(
        inner.engram_session_obligations.entries[0].routing_token,
        "retained-token"
    );
    let duty = &inner.engram_session_obligations.entries[0];
    assert_eq!(duty.phase, EngramSessionObligationPhase::Sent);
    assert!(duty.settlement_evidence.is_null());
    assert!(!inner.engram_confirmed_containment_ids.contains(&duty.id));
    assert_eq!(record.engram.dispatch_generation, duty.owner_generation + 1);
    assert_eq!(inner.engram_pending_checkpoint_receipts.entries.len(), 1);
    let pending = inner
        .engram_pending_checkpoint_receipts
        .entries
        .values()
        .next()
        .unwrap();
    assert!(pending.origin == EngramCheckpointReceiptOrigin::OffContainment);
    assert!(pending.accepted);
    assert!(!pending.accounting);
    let owner: EngramContainmentReceiptOwner = serde_json::from_slice(&pending.identity).unwrap();
    assert!(off_checkpoint_original_matches(duty, &owner.obligation).unwrap());
    assert_eq!(owner.obligation.id, duty.id);
    assert_eq!(owner.obligation.routing_token, "retained-token");
    assert_eq!(owner.obligation.grant_id, "retained-grant");
    assert_eq!(
        serde_json::from_slice::<Value>(pending.response.as_ref().unwrap()).unwrap(),
        json!({"decision":"checkpointed", "receipt":{
            "grant_id":"retained-grant", "cursor":1, "confirmed_cursor":1,
        }})
    );
    assert!(require_no_engram_session_obligation(&inner, &session).is_err());
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_missing_writer_receipt_keeps_exclusion_and_sends_nothing() {
    let (state, project, session, _root, receiver) = fixture();
    let transport = ScriptedEngramControlTransport::new([checkpoint_reply("retained-grant")]);
    install_control_only_transport(&state, transport.clone());
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(false));
    let result = state.update_project_engram_settings(&project, settings(&state, &project, false));
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(true));
    assert!(result.is_err());
    let inner = state.inner.lock().unwrap();
    assert!(require_no_engram_session_obligation(&inner, &session).is_err());
    assert!(inner.engram_confirmed_containment_ids.is_empty());
    assert!(!inner.engram_project_resets.contains(&project));
    assert!(transport.requests().is_empty());
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_existing_unconfirmed_stop_is_not_reported_settled() {
    let (state, project, session, _root, receiver) = fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.session.status = SessionStatus::Stopping;
        record.runtime_stop_in_progress = true;
        state.commit_locked(&mut inner).unwrap();
    }
    let transport = ScriptedEngramControlTransport::new([]);
    install_control_only_transport(&state, transport.clone());
    let response = state
        .update_project_engram_settings(&project, settings(&state, &project, false))
        .unwrap();
    assert_eq!(
        response.engram_control_recovery[0].code,
        "provider_interruption_unconfirmed"
    );
    assert!(!response.engram_control_recovery[0].settlement_confirmed);
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.session.status, SessionStatus::Stopping);
    assert!(record.runtime_stop_in_progress);
    assert_eq!(
        record.engram.active_grant_id.as_deref(),
        Some("retained-grant")
    );
    assert!(require_no_engram_session_obligation(&inner, &session).is_err());
    assert!(transport.requests().is_empty());
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_recovery_capacity_refuses_before_settings_or_rpc_effects() {
    let (state, project, session, _root, receiver) = fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.engram.uncertain_grant_id = Some("possibly-sent".to_owned());
        queue_prompt_on_record(
            record,
            PendingPrompt {
                id: "original-large-head".to_owned(),
                timestamp: stamp_now(),
                text: "x".repeat(ENGRAM_SESSION_OBLIGATION_REQUEST_MAX_BYTES + 1),
                expanded_text: None,
                source: None,
                attachments: Vec::new(),
                engram_interrupted: true,
                is_engram_retained: true,
            },
            Vec::new(),
        );
        state.commit_locked(&mut inner).unwrap();
    }
    let transport = ScriptedEngramControlTransport::new([]);
    install_control_only_transport(&state, transport.clone());
    assert!(
        state
            .update_project_engram_settings(&project, settings(&state, &project, false))
            .is_err()
    );
    let inner = state.inner.lock().unwrap();
    assert!(
        inner
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap()
            .turn_gated_control
    );
    assert!(!inner.engram_project_resets.contains(&project));
    assert!(inner.engram_session_obligations.entries.is_empty());
    assert!(transport.requests().is_empty());
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_cancelled_head_and_local_recreation_keep_original_obligation() {
    let (state, project, session, _root, receiver) = fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        queue_prompt_on_record(
            inner.session_mut_by_index(index).unwrap(),
            PendingPrompt {
                id: "original-held-head".to_owned(),
                timestamp: stamp_now(),
                text: "Original queued recovery".to_owned(),
                expanded_text: None,
                source: None,
                attachments: Vec::new(),
                engram_interrupted: false,
                is_engram_retained: false,
            },
            Vec::new(),
        );
        state.commit_locked(&mut inner).unwrap();
    }
    install_control_only_transport(
        &state,
        ScriptedEngramControlTransport::new([checkpoint_refusal_reply("GrantScopeMismatch")]),
    );
    state
        .update_project_engram_settings(&project, settings(&state, &project, false))
        .unwrap();
    let original = {
        let inner = state.inner.lock().unwrap();
        serde_json::to_value(&inner.engram_session_obligations).unwrap()
    };
    state
        .cancel_queued_prompt(&session, "original-held-head")
        .unwrap();
    {
        let inner = state.inner.lock().unwrap();
        assert!(require_no_engram_session_obligation(&inner, &session).is_err(),
            "Cancel is not settlement of the original control duty");
        assert_eq!(serde_json::to_value(&inner.engram_session_obligations).unwrap(), original);
        assert!(require_no_engram_local_dispatch_obligation(&inner, &session).is_ok(),
            "only direct fresh intake with no older queued work may use the OFF exception");
    }
    assert!(state.resume_local_session_queue(&session, None).is_err());
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        // Even a recreated local generation with no retained row fields
        // cannot discard the host-level original recovery identity.
        record.engram = EngramSessionState::default();
        assert!(require_no_engram_session_obligation(&inner, &session).is_err());
        assert_eq!(
            serde_json::to_value(&inner.engram_session_obligations).unwrap(),
            original
        );
        state.commit_locked(&mut inner).unwrap();
    }
    {
        let inner = state.inner.lock().unwrap();
        assert!(require_no_engram_session_obligation(&inner, &session).is_err());
        assert_eq!(serde_json::to_value(&inner.engram_session_obligations).unwrap(), original);
        assert!(require_no_engram_local_dispatch_obligation(&inner, &session).is_ok());
    }
    assert!(receiver.try_recv().is_err());
    state.shutdown_persist_blocking();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(require_no_engram_session_obligation(&loaded, &session).is_err());
    assert_eq!(
        serde_json::to_value(&loaded.engram_session_obligations).unwrap(),
        original
    );
}
