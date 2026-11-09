//! Ordinary delegation creation needs no unqueued Bind phase budget.
use super::uncertain_begin::OrdinaryRoot;
use super::*;

#[test]
fn ordinary_read_only_delegation_starts_and_cancels_without_control_calls() {
    let turn = OrdinaryRoot::new("ordinary-delegation-no-bind");
    let created = turn
        .state
        .create_read_only_delegation(
            &turn.session,
            CreateDelegationRequest {
                prompt: "ordinary delegated work".into(),
                title: Some("ordinary child".into()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Explorer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .unwrap();
    let child = &created.delegation.child_session_id;
    let mut prompts = 0;
    while let Ok(command) = turn.receiver.try_recv() {
        if let CodexRuntimeCommand::Prompt {
            session_id,
            command,
        } = command
        {
            assert_eq!(&session_id, child);
            assert!(command.prompt.contains("ordinary delegated work"));
            prompts += 1;
        }
    }
    assert_eq!(prompts, 1);
    turn.state
        .cancel_delegation(&turn.session, &created.delegation.id)
        .unwrap();
    let result = turn
        .state
        .get_delegation(&turn.session, &created.delegation.id)
        .unwrap();
    assert_eq!(result.delegation.status, DelegationStatus::Canceled);
    let inner = turn.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(child).unwrap()];
    assert!(record.queued_prompts.is_empty());
    drop(inner);
    turn.assert_no_control();
}
