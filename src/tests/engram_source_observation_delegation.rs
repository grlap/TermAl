//! Automatic observation retries must refresh the parent's public attempt.
use super::*;

fn attach_observation_delegation(claimed: &ClaimedRoot) -> (String, String) {
    let (parent, id) = {
        let mut inner = claimed.state.inner.lock().unwrap();
        let parent = inner.create_session(Agent::Codex, Some("Observation parent".to_owned()),
            claimed.root.to_string_lossy().into_owned(), None, None).session.id.clone();
        let id = inner.next_delegation_id();
        let record: DelegationRecord = serde_json::from_value(json!({
            "id": id, "parentSessionId": parent, "childSessionId": claimed.session_id,
            "mode": "reviewer", "status": "running", "title": "Observation child",
            "prompt": "Original retained prompt", "cwd": claimed.root,
            "agent": "Codex", "writePolicy": {"kind": "readOnly"}, "createdAt": stamp_now()
        })).unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        inner.sessions[index].session.parent_delegation_id = Some(id.clone());
        add_parent_delegation_card_locked(&mut inner, &record).unwrap();
        inner.delegations.push(record);
        claimed.state.commit_locked(&mut inner).unwrap();
        (parent, id)
    };
    claimed.state.sync_delegation_attempt_for_child_session(&claimed.session_id);
    assert_eq!(public_delegation_status(&claimed.state.inner.lock().unwrap().delegations[0]),
        DelegationStatus::Held);
    (parent, id)
}

#[test]
fn source_observation_delegation_retry_refreshes_success() {
    let claimed = held_observation("source-observation-delegation-success");
    let (parent, id) = attach_observation_delegation(&claimed);
    claimed.state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::seconds(2));
    let _ = received_prompt(&claimed);
    let guard = super::super::super::super::phase_sync::PollGuard::new();
    loop {
        let inner = claimed.state.inner.lock().unwrap();
        let record = &inner.delegations[inner.find_delegation_index(&id).unwrap()];
        if public_delegation_status(record) == DelegationStatus::Running && !record.attempt.pending_start {
            assert!(record.attempt.hold.is_none());
            let detail = delegation_running_detail_locked(&inner, record);
            assert!(parent_delegation_card_matches_locked(&inner, record, ParallelAgentStatus::Running, &detail));
            assert_eq!(record.parent_session_id, parent);
            break;
        }
        drop(inner);
        guard.wait(format_args!("accepted observation retry must clear the parent's Held state"));
    }
}

#[test]
fn source_observation_delegation_retry_refreshes_rehold_and_exhaustion() {
    for exhausted in [false, true] {
        let claimed = held_observation(if exhausted {
            "source-observation-delegation-exhausted"
        } else { "source-observation-delegation-rehold" });
        let (_, id) = attach_observation_delegation(&claimed);
        claimed.record(|record| {
            record.session.preview = "A newly diagnosed observation hold".to_owned();
            if exhausted { record.engram.source_observation_gate.as_mut().unwrap().attempts = 8; }
        });
        if !exhausted {
            claimed.transport.named_roots.lock().unwrap().as_mut().unwrap().lose_next_observation_reply = true;
        }
        claimed.state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::seconds(2));
        let guard = super::super::super::super::phase_sync::PollGuard::new();
        loop {
            let inner = claimed.state.inner.lock().unwrap();
            let child = &inner.sessions[inner.find_session_index(&claimed.session_id).unwrap()];
            let record = &inner.delegations[inner.find_delegation_index(&id).unwrap()];
            if child.engram.admission_in_progress.is_none()
                && record.attempt.hold.as_ref().is_some_and(|hold| hold.detail == child.session.preview) {
                assert_eq!(public_delegation_status(record), DelegationStatus::Held);
                let detail = delegation_hold_card_detail(record.attempt.hold.as_ref().unwrap());
                assert!(parent_delegation_card_matches_locked(&inner, record, ParallelAgentStatus::Running, &detail),
                    "exhausted={exhausted}; expected={detail:?}; actual={:?}",
                    inner.sessions[inner.find_session_index(&record.parent_session_id).unwrap()].session.messages);
                if exhausted { assert!(child.engram.source_observation_gate.as_ref().unwrap().retired); }
                break;
            }
            let detail = format!("observation retry hold/exhaustion must refresh the parent's guidance: exhausted={exhausted}, child={:?}, admitting={}, preview={:?}, parent_hold={:?}, gate={:?}",
                child.session.status, child.engram.admission_in_progress.is_some(), child.session.preview,
                record.attempt.hold, child.engram.source_observation_gate);
            drop(inner);
            guard.wait(format_args!("{detail}"));
        }
        assert!(claimed.runtime_rx.try_recv().is_err());
    }
}
