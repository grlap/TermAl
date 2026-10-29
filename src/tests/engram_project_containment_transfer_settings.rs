// Owns current-settings fences around an unacknowledged admission transfer.
// Does not settle independent duties or infer delivery from local persistence.
use super::*;

struct LoseFinalTransferAcknowledgement {
    inner: Arc<LostContainmentAdmissionReply>,
    armed: AtomicBool,
}

impl EngramControlTransport for LoseFinalTransferAcknowledgement {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        let result = self.inner.request(connection, request, timeout);
        if result.is_ok()
            && matches!(request, EngramControlRequest::TurnEvaluate { .. })
            && self.armed.swap(false, Ordering::SeqCst)
        {
            // The public Resume's original owner ACK has already completed.
            // Lose only the following final-transfer writer acknowledgement.
            TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(false));
        }
        result
    }

    fn shutdown_session(&self, session_id: &str) {
        self.inner.shutdown_session(session_id);
    }
}

fn unacknowledged_transfer() -> (
    AppState,
    String,
    String,
    mpsc::Receiver<CodexRuntimeCommand>,
    Arc<LostContainmentAdmissionReply>,
) {
    let (state, project, session, _root, receiver, transport) =
        retained_admission(&["turn_evaluate"]);
    state
        .update_project_engram_settings(&project, settings(&state, &project, true))
        .unwrap();
    state.install_control_test_transport(Arc::new(LoseFinalTransferAcknowledgement {
        inner: transport.clone(),
        armed: AtomicBool::new(true),
    }));
    let _ = state.resume_session_queue(&session);
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(true));
    {
        let inner = state.inner.lock().unwrap();
        let entry = engram_session_obligation_locked(&inner, &session)
            .expect("lost final acknowledgement retains the exclusion");
        assert_eq!(entry.phase, EngramSessionObligationPhase::Confirmed);
        assert!(containment_admission_transfer_matches(entry));
        assert!(!inner.engram_confirmed_containment_ids.contains(&entry.id));
        assert!(
            inner.sessions[inner.find_session_index(&session).unwrap()].queued_prompts[0]
                .engram_evaluate
                .as_ref()
                .unwrap()
                .prepared_begin
                .is_some()
        );
    }
    assert!(
        !transport
            .inner
            .requests()
            .iter()
            .any(|request| { request.request["operation"] == "turn_begin" })
    );
    assert!(receiver.try_recv().is_err());
    (state, project, session, receiver, transport)
}

fn off_after_lost_transfer_ack_stays_excluded(reload: bool) {
    let (state, project, session, receiver, transport) = unacknowledged_transfer();
    let before = transport.inner.requests().len();
    let response = state
        .update_project_engram_settings(&project, settings(&state, &project, false))
        .unwrap();
    assert_eq!(response.engram_control_recovery.len(), 1);
    if reload {
        state.shutdown_persist_blocking();
        let loaded = load_state(state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        assert!(
            engram_session_obligation_locked(&loaded, &session).is_some(),
            "boot cannot acknowledge an issued successor under gating OFF"
        );
        assert!(loaded.engram_confirmed_containment_ids.is_empty());
    } else {
        let _ = state.resume_session_queue(&session);
        let inner = state.inner.lock().unwrap();
        assert!(
            engram_session_obligation_locked(&inner, &session).is_some(),
            "Resume cannot acknowledge an issued successor under gating OFF"
        );
        assert!(inner.engram_confirmed_containment_ids.is_empty());
        drop(inner);
        assert_eq!(state.snapshot().engram_control_recovery.len(), 1);
        assert!(state.kill_session(&session).is_err());
    }
    assert_eq!(
        transport.inner.requests().len(),
        before,
        "OFF recovery sends no RPC"
    );
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_lost_transfer_ack_then_off_resume_keeps_exclusion() {
    off_after_lost_transfer_ack_stays_excluded(false);
}

#[test]
fn project_containment_lost_transfer_ack_then_off_restart_keeps_exclusion() {
    off_after_lost_transfer_ack_stays_excluded(true);
}

#[test]
fn project_containment_lost_transfer_ack_exact_reenable_allows_local_confirmation() {
    let (state, project, session, receiver, transport) = unacknowledged_transfer();
    let before = transport.inner.requests().len();
    state
        .update_project_engram_settings(&project, settings(&state, &project, false))
        .unwrap();
    state
        .update_project_engram_settings(&project, settings(&state, &project, true))
        .unwrap();
    state
        .retry_containment_admission_transfer(&session)
        .unwrap();
    let inner = state.inner.lock().unwrap();
    assert!(engram_session_obligation_locked(&inner, &session).is_none());
    let entry = &inner.engram_session_obligations.entries[0];
    assert!(inner.engram_confirmed_containment_ids.contains(&entry.id));
    assert!(containment_admission_transfer_matches(entry));
    assert_eq!(
        transport.inner.requests().len(),
        before,
        "local ACK sends no RPC"
    );
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_transfer_settings_changed_during_ack_cannot_release_permission() {
    let (state, project, session, receiver, transport) = unacknowledged_transfer();
    let before = transport.inner.requests().len();
    let off_state = state.clone();
    let off_project = project.clone();
    TEST_CONTAINMENT_BEFORE_ACK_RETURN.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            off_state
                .update_project_engram_settings(
                    &off_project,
                    settings(&off_state, &off_project, false),
                )
                .unwrap();
        }));
    });
    assert!(
        state
            .retry_containment_admission_transfer(&session)
            .is_err()
    );
    let inner = state.inner.lock().unwrap();
    let entry = engram_session_obligation_locked(&inner, &session).unwrap();
    assert!(!inner.engram_confirmed_containment_ids.contains(&entry.id));
    assert!(
        !inner
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap()
            .turn_gated_control
    );
    assert!(
        inner.sessions[inner.find_session_index(&session).unwrap()]
            .queued_prompts
            .front()
            .unwrap()
            .engram_evaluate
            .as_ref()
            .unwrap()
            .prepared_begin
            .is_some()
    );
    assert_eq!(transport.inner.requests().len(), before);
    assert!(receiver.try_recv().is_err());
}
