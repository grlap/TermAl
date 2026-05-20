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

fn push_text_message(inner: &mut StateInner, session_id: &str, author: Author, text: String) {
    let message_id = inner.next_message_id();
    let session_index = inner
        .find_session_index(session_id)
        .expect("session should exist");
    let session = inner
        .session_mut_by_index(session_index)
        .expect("session index should be valid");
    push_message_on_record(
        session,
        Message::Text {
            attachments: Vec::new(),
            id: message_id,
            timestamp: stamp_now(),
            author,
            text,
            expanded_text: None,
        },
    );
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
fn repair_delegation_child_session_links_backfills_orphaned_marker_child() {
    let (mut inner, parent_id, child_id) = delegation_link_inner();
    let delegation_id = "delegation-marker-link-test";
    let prompt = build_delegation_prompt(&test_delegation_record(
        delegation_id,
        &parent_id,
        &child_id,
    ));
    push_text_message(&mut inner, &child_id, Author::You, prompt);
    let child_index = inner
        .find_session_index(&child_id)
        .expect("child session should exist");
    inner.sessions[child_index].session.parent_delegation_id = None;
    let before_repair_stamp = inner.sessions[child_index].mutation_stamp;

    inner.repair_delegation_child_session_links();

    let child_index = inner
        .find_session_index(&child_id)
        .expect("child session should still exist");
    assert!(
        inner.delegations.is_empty(),
        "test must cover orphaned child sessions without delegation rows"
    );
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
fn delegation_marker_parser_requires_matching_child_session_id() {
    let (_inner, parent_id, child_id) = delegation_link_inner();
    let delegation_id = "delegation-marker-parser-test";
    let prompt = build_delegation_prompt(&test_delegation_record(
        delegation_id,
        &parent_id,
        &child_id,
    ));

    assert_eq!(
        delegation_id_from_delegated_child_marker(&prompt, &child_id).as_deref(),
        Some(delegation_id)
    );
    assert_eq!(
        delegation_id_from_delegated_child_marker(&prompt, "session-other").as_deref(),
        None
    );
}

#[test]
fn delegation_marker_parser_rejects_bare_delegation_prefix() {
    let (_inner, _parent_id, child_id) = delegation_link_inner();
    let prompt =
        format!("{DELEGATED_CHILD_SESSION_MARKER} `delegation-`.\n\nChild session: `{child_id}`\n");

    assert_eq!(
        delegation_id_from_delegated_child_marker(&prompt, &child_id).as_deref(),
        None
    );
}

#[test]
fn delegation_marker_parser_rejects_malformed_prompt_tail() {
    let (_inner, _parent_id, child_id) = delegation_link_inner();
    let delegation_id = "delegation-malformed-tail-test";
    let cases = [
        format!(
            "{DELEGATED_CHILD_SESSION_MARKER} `{delegation_id}`\n\nChild session: `{child_id}`\n"
        ),
        format!("{DELEGATED_CHILD_SESSION_MARKER} `{delegation_id}`."),
        format!(
            "{DELEGATED_CHILD_SESSION_MARKER} `{delegation_id}`.\n\nChild session: `{child_id}` extra\n"
        ),
        format!("{DELEGATED_CHILD_SESSION_MARKER} `{delegation_id}`.\n\nMode: Reviewer\n"),
        format!(
            "{DELEGATED_CHILD_SESSION_MARKER} `{delegation_id}.\n\nChild session: `{child_id}`\n"
        ),
    ];

    for prompt in cases {
        assert_eq!(
            delegation_id_from_delegated_child_marker(&prompt, &child_id).as_deref(),
            None,
            "prompt should be rejected: {prompt:?}"
        );
    }
}

#[test]
fn repair_delegation_child_session_links_ignores_quoted_marker_in_regular_session() {
    let (mut inner, _parent_id, child_id) = delegation_link_inner();
    push_text_message(
        &mut inner,
        &child_id,
        Author::You,
        format!(
            "The marker parser used to scan for `{DELEGATED_CHILD_SESSION_MARKER}` and extract the next backticked id. It must not treat `and extracting the next backticked id` as a delegation id."
        ),
    );
    let child_index = inner
        .find_session_index(&child_id)
        .expect("session should exist");
    let before_repair_stamp = inner.sessions[child_index].mutation_stamp;

    inner.repair_delegation_child_session_links();

    let child_index = inner
        .find_session_index(&child_id)
        .expect("session should still exist");
    assert_eq!(
        inner.sessions[child_index]
            .session
            .parent_delegation_id
            .as_deref(),
        None
    );
    assert_eq!(
        inner.sessions[child_index].mutation_stamp,
        before_repair_stamp
    );
}

#[test]
fn repair_delegation_child_session_links_ignores_later_quoted_bootstrap_prompt() {
    let (mut inner, parent_id, child_id) = delegation_link_inner();
    let prompt = build_delegation_prompt(&test_delegation_record(
        "delegation-quoted-prompt-test",
        &parent_id,
        &child_id,
    ));
    push_text_message(
        &mut inner,
        &child_id,
        Author::You,
        "Can you review this delegated prompt?".to_owned(),
    );
    push_text_message(&mut inner, &child_id, Author::You, prompt);
    let child_index = inner
        .find_session_index(&child_id)
        .expect("session should exist");
    let before_repair_stamp = inner.sessions[child_index].mutation_stamp;

    inner.repair_delegation_child_session_links();

    let child_index = inner
        .find_session_index(&child_id)
        .expect("session should still exist");
    assert_eq!(
        inner.sessions[child_index]
            .session
            .parent_delegation_id
            .as_deref(),
        None
    );
    assert_eq!(
        inner.sessions[child_index].mutation_stamp,
        before_repair_stamp
    );
}

#[test]
fn repair_delegation_child_session_links_ignores_wrong_child_session_marker() {
    let (mut inner, parent_id, child_id) = delegation_link_inner();
    let prompt = build_delegation_prompt(&test_delegation_record(
        "delegation-wrong-child-test",
        &parent_id,
        "session-other",
    ));
    push_text_message(&mut inner, &child_id, Author::You, prompt);
    let child_index = inner
        .find_session_index(&child_id)
        .expect("session should exist");
    let before_repair_stamp = inner.sessions[child_index].mutation_stamp;

    inner.repair_delegation_child_session_links();

    let child_index = inner
        .find_session_index(&child_id)
        .expect("session should still exist");
    assert_eq!(
        inner.sessions[child_index]
            .session
            .parent_delegation_id
            .as_deref(),
        None
    );
    assert_eq!(
        inner.sessions[child_index].mutation_stamp,
        before_repair_stamp
    );
}

#[test]
fn repair_delegation_child_session_links_ignores_non_user_or_markdown_markers() {
    let (mut inner, parent_id, child_id) = delegation_link_inner();
    let markdown_child = inner.create_session(
        Agent::Claude,
        Some("Markdown Child".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let markdown_child_id = markdown_child.session.id;
    let assistant_prompt = build_delegation_prompt(&test_delegation_record(
        "delegation-assistant-marker-test",
        &parent_id,
        &child_id,
    ));
    push_text_message(&mut inner, &child_id, Author::Assistant, assistant_prompt);
    let markdown_message_id = inner.next_message_id();
    let markdown_prompt = build_delegation_prompt(&test_delegation_record(
        "delegation-markdown-marker-test",
        &parent_id,
        &markdown_child_id,
    ));
    let markdown_child_index = inner
        .find_session_index(&markdown_child_id)
        .expect("markdown session should exist");
    {
        let child = inner
            .session_mut_by_index(markdown_child_index)
            .expect("markdown session index should be valid");
        push_message_on_record(
            child,
            Message::Markdown {
                id: markdown_message_id,
                timestamp: stamp_now(),
                author: Author::You,
                title: "Quoted prompt".to_owned(),
                markdown: markdown_prompt,
            },
        );
    }
    let child_index = inner
        .find_session_index(&child_id)
        .expect("session should exist");
    let before_repair_stamp = inner.sessions[child_index].mutation_stamp;
    let before_markdown_repair_stamp = inner.sessions[markdown_child_index].mutation_stamp;

    inner.repair_delegation_child_session_links();

    let child_index = inner
        .find_session_index(&child_id)
        .expect("session should still exist");
    assert_eq!(
        inner.sessions[child_index]
            .session
            .parent_delegation_id
            .as_deref(),
        None
    );
    assert_eq!(
        inner.sessions[child_index].mutation_stamp,
        before_repair_stamp
    );
    let markdown_child_index = inner
        .find_session_index(&markdown_child_id)
        .expect("markdown session should still exist");
    assert_eq!(
        inner.sessions[markdown_child_index]
            .session
            .parent_delegation_id
            .as_deref(),
        None
    );
    assert_eq!(
        inner.sessions[markdown_child_index].mutation_stamp,
        before_markdown_repair_stamp
    );
}

#[test]
fn repair_delegation_child_session_links_clears_invalid_false_positive_parent_id() {
    let (mut inner, _parent_id, child_id) = delegation_link_inner();
    let child_index = inner
        .find_session_index(&child_id)
        .expect("session should exist");
    inner.sessions[child_index].session.parent_delegation_id =
        Some("and extracting the next backticked id".to_owned());
    let before_repair_stamp = inner.sessions[child_index].mutation_stamp;

    inner.repair_delegation_child_session_links();

    let child_index = inner
        .find_session_index(&child_id)
        .expect("session should still exist");
    assert_eq!(
        inner.sessions[child_index]
            .session
            .parent_delegation_id
            .as_deref(),
        None
    );
    assert!(inner.sessions[child_index].mutation_stamp > before_repair_stamp);
}

#[test]
fn repair_delegation_child_session_links_keeps_unmatched_valid_parent_id() {
    let (mut inner, _parent_id, child_id) = delegation_link_inner();
    let child_index = inner
        .find_session_index(&child_id)
        .expect("session should exist");
    inner.sessions[child_index].session.parent_delegation_id =
        Some("delegation-existing-unmatched".to_owned());
    let before_repair_stamp = inner.sessions[child_index].mutation_stamp;

    inner.repair_delegation_child_session_links();

    let child_index = inner
        .find_session_index(&child_id)
        .expect("session should still exist");
    assert_eq!(
        inner.sessions[child_index]
            .session
            .parent_delegation_id
            .as_deref(),
        Some("delegation-existing-unmatched")
    );
    assert_eq!(
        inner.sessions[child_index].mutation_stamp,
        before_repair_stamp
    );
}

#[test]
fn persisted_state_reload_hides_orphaned_marker_child() {
    let (mut inner, parent_id, child_id) = delegation_link_inner();
    let delegation_id = "delegation-marker-reload-test";
    let prompt = build_delegation_prompt(&test_delegation_record(
        delegation_id,
        &parent_id,
        &child_id,
    ));
    push_text_message(&mut inner, &child_id, Author::You, prompt);
    let child_index = inner
        .find_session_index(&child_id)
        .expect("child session should exist");
    inner.sessions[child_index].session.parent_delegation_id = None;

    let reloaded = PersistedState::from_inner(&inner)
        .into_inner()
        .expect("persisted state should reload");
    let child_index = reloaded
        .find_session_index(&child_id)
        .expect("child session should reload");

    assert!(
        reloaded.delegations.is_empty(),
        "test must cover startup repair without delegation rows"
    );
    assert_eq!(
        reloaded.sessions[child_index]
            .session
            .parent_delegation_id
            .as_deref(),
        Some(delegation_id)
    );
}

#[test]
fn persisted_state_reload_keeps_regular_session_visible_when_marker_is_quoted() {
    let (mut inner, parent_id, child_id) = delegation_link_inner();
    let prompt = build_delegation_prompt(&test_delegation_record(
        "delegation-reload-quoted-prompt-test",
        &parent_id,
        &child_id,
    ));
    push_text_message(
        &mut inner,
        &child_id,
        Author::You,
        "Please inspect this prompt shape.".to_owned(),
    );
    push_text_message(&mut inner, &child_id, Author::You, prompt);

    let reloaded = PersistedState::from_inner(&inner)
        .into_inner()
        .expect("persisted state should reload");
    let child_index = reloaded
        .find_session_index(&child_id)
        .expect("session should reload");

    assert_eq!(
        reloaded.sessions[child_index]
            .session
            .parent_delegation_id
            .as_deref(),
        None
    );
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
