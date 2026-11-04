// Owns completion of the actual asynchronous settlement, plus its stale-selection
// interleave. Content durability and provider handoff remain separate fences.

pub(super) fn observe_settlement_completion() -> mpsc::Receiver<()> {
    let (tx, rx) = mpsc::channel();
    TEST_QUEUED_PROMPT_SETTLEMENT.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Arc::new(move |phase| {
            if phase == "released" {
                let _ = tx.send(());
            }
        }));
    });
    rx
}

struct ReleaseSettlement(Arc<(Mutex<bool>, Condvar)>);

impl Drop for ReleaseSettlement {
    fn drop(&mut self) {
        phase_sync::set_gate(&self.0, true);
    }
}

#[test]
fn selected_resume_rejects_the_snapshot_taken_before_settlement_finishes() {
    let (mut state, _project, session, transport, receiver) = ordinary_receipt_fixture(true, true);
    let writer = ReceiptWriter::attach(&mut state);
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let release = ReleaseSettlement(gate.clone());
    let (phases_tx, phases_rx) = mpsc::channel();
    let observed_state = state.clone();
    let observed_session = session.clone();
    TEST_QUEUED_PROMPT_SETTLEMENT.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Arc::new(move |phase| {
            let inner = observed_state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&observed_session).unwrap()];
            let observed = (phase.to_owned(), record.mutation_stamp);
            drop(inner);
            phases_tx.send(observed).unwrap();
            if phase == "acknowledged" {
                phase_sync::wait_for_gate(&gate, true);
            }
        }));
    });
    assert!(state.request_queued_prompt_durability(&session, "queued-before-off"));
    let acknowledged = phase_sync::receive(&phases_rx, "content ACK before its release attempt");
    assert_eq!(acknowledged.0, "acknowledged");
    let (selection, queue, stored_history) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.queued_prompt_durability_holds["queued-before-off"].acknowledged);
        assert!(record.orchestrator_auto_dispatch_blocked);
        (
            HumanPromptSelection {
                prompt: projected_pending_prompts(record).remove(0),
                session_mutation_stamp: record.mutation_stamp,
            },
            serde_json::to_value(&record.queued_prompts).unwrap(),
            serde_json::to_value(&record.engram.historical_control).unwrap(),
        )
    };
    assert_eq!(selection.session_mutation_stamp, acknowledged.1);
    drop(release);
    let released = phase_sync::receive(&phases_rx, "full settlement release attempt");
    assert_eq!(released.0, "released");
    assert!(
        released.1 > acknowledged.1,
        "the completed release attempt invalidates the earlier snapshot"
    );
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(serde_json::to_value(&record.queued_prompts).unwrap(), queue);
        assert_eq!(
            serde_json::to_value(&record.engram.historical_control).unwrap(),
            stored_history
        );
        assert!(record.orchestrator_auto_dispatch_blocked);
    }
    let error = state
        .resume_selected_human_prompt(&session, selection)
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(
        error.message,
        "the selected queued prompt changed; refresh before resuming"
    );
    assert!(receiver.try_recv().is_err());
    assert!(transport.requests().is_empty());
    drop(writer);
}
