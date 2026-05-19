use super::*;

fn test_delegation_record(
    id: &str,
    parent_session_id: &str,
    child_session_id: &str,
) -> DelegationRecord {
    DelegationRecord {
        id: id.to_owned(),
        parent_session_id: parent_session_id.to_owned(),
        child_session_id: child_session_id.to_owned(),
        mode: DelegationMode::Reviewer,
        status: DelegationStatus::Running,
        title: "Reviewer".to_owned(),
        prompt: "Review the patch.".to_owned(),
        cwd: "/tmp".to_owned(),
        agent: Agent::Codex,
        model: None,
        write_policy: DelegationWritePolicy::ReadOnly,
        created_at: stamp_now(),
        started_at: None,
        completed_at: None,
        result: None,
    }
}

fn delegation_link_inner() -> (StateInner, String, String) {
    let mut inner = StateInner::new();
    let parent = inner.create_session(
        Agent::Codex,
        Some("Parent".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let child = inner.create_session(
        Agent::Claude,
        Some("Child".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let parent_id = parent.session.id;
    let child_id = child.session.id;
    (inner, parent_id, child_id)
}

#[test]
fn repair_delegation_child_session_links_backfills_missing_link() {
    let (mut inner, parent_id, child_id) = delegation_link_inner();
    let delegation_id = "delegation-link-test";

    inner
        .delegations
        .push(test_delegation_record(delegation_id, &parent_id, &child_id));
    inner.delegations.push(test_delegation_record(
        "delegation-missing-child",
        &parent_id,
        "session-missing",
    ));

    let child_index = inner
        .find_session_index(&child_id)
        .expect("child session should exist");
    inner.sessions[child_index].session.parent_delegation_id = None;
    let before_repair_stamp = inner.sessions[child_index].mutation_stamp;

    inner.repair_delegation_child_session_links();

    let child_index = inner
        .find_session_index(&child_id)
        .expect("child session should still exist");
    assert_eq!(
        inner.sessions[child_index]
            .session
            .parent_delegation_id
            .as_deref(),
        Some(delegation_id)
    );
    assert!(inner.sessions[child_index].mutation_stamp > before_repair_stamp);
}

#[test]
fn repair_delegation_child_session_links_skips_already_correct_link_without_bumping_stamp() {
    let (mut inner, parent_id, child_id) = delegation_link_inner();
    let delegation_id = "delegation-link-test";
    inner
        .delegations
        .push(test_delegation_record(delegation_id, &parent_id, &child_id));
    let child_index = inner
        .find_session_index(&child_id)
        .expect("child session should exist");
    inner.sessions[child_index].session.parent_delegation_id = Some(delegation_id.to_owned());
    let repaired_stamp = inner.sessions[child_index].mutation_stamp;

    inner.repair_delegation_child_session_links();

    assert_eq!(inner.sessions[child_index].mutation_stamp, repaired_stamp);
}

#[test]
fn repair_delegation_child_session_links_skips_missing_child_sessions() {
    let (mut inner, parent_id, child_id) = delegation_link_inner();
    inner.delegations.push(test_delegation_record(
        "delegation-missing-child",
        &parent_id,
        "session-missing",
    ));
    let session_ids_before = inner
        .sessions
        .iter()
        .map(|record| record.session.id.clone())
        .collect::<Vec<_>>();

    inner.repair_delegation_child_session_links();

    assert_eq!(
        inner
            .find_session_index(&child_id)
            .and_then(|index| inner.sessions[index]
                .session
                .parent_delegation_id
                .as_deref()),
        None
    );
    assert_eq!(
        inner
            .sessions
            .iter()
            .map(|record| record.session.id.clone())
            .collect::<Vec<_>>(),
        session_ids_before
    );
}
