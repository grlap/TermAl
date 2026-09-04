//! Split-lock persist-delta planning, materialization, retry, and merge coverage.
//!
//! These tests keep concurrency-boundary invariants together: stable rows are
//! not rewritten, changed candidates are retried, tombstones remain
//! authoritative, and selected prompt history survives deferral.

use super::*;

fn make_persist_delta_test_delegation(
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
        title: "Persisted Delegation".to_owned(),
        prompt: "/review-code".to_owned(),
        cwd: "/tmp".to_owned(),
        agent: Agent::Codex,
        model: None,
        write_policy: DelegationWritePolicy::ReadOnly,
        created_at: stamp_now(),
        started_at: Some(stamp_now()),
        completed_at: None,
        result: None,
        submitted_review_result: None,
        post_submission_transport_error: None,
        review_result_recovery_probe_attempt: None,
        review_result_recovery_error: None,
        review_result_schema_version: None,
        review_result_submission_attempt: 0,
    }
}

#[test]
fn persist_delta_plan_defers_only_the_session_that_changes_before_materialization() {
    let mut inner = StateInner::new();
    let session_id = inner
        .create_session(
            Agent::Claude,
            Some("Changing snapshot".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let session_index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    inner
        .session_mut_by_index(session_index)
        .expect("session should be mutable")
        .session
        .preview = "selected version".to_owned();
    let stable_session_id = inner
        .create_session(
            Agent::Claude,
            Some("Stable snapshot".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let stable_session_index = inner
        .find_session_index(&stable_session_id)
        .expect("stable session should exist");
    inner
        .session_mut_by_index(stable_session_index)
        .expect("stable session should be mutable")
        .session
        .preview = "stable version".to_owned();

    let plan = inner.collect_persist_delta_plan(0);
    let selected_watermark = plan.watermark;
    assert!(selected_watermark > 0);

    inner
        .session_mut_by_index(session_index)
        .expect("session should remain mutable")
        .session
        .preview = "newer version".to_owned();

    let deferred = inner.materialize_persist_delta(plan);
    assert_eq!(
        deferred
            .changed_sessions
            .iter()
            .map(|record| record.session.id.as_str())
            .collect::<Vec<_>>(),
        vec![stable_session_id.as_str()],
        "stable candidates should persist while the newer session is deferred"
    );
    assert_eq!(
        deferred.watermark, selected_watermark,
        "the worker should advance past stable snapshots without losing a later mutation"
    );

    let retry = inner.collect_persist_delta(deferred.watermark);
    assert!(retry.watermark > selected_watermark);
    assert_eq!(retry.changed_sessions.len(), 1);
    assert_eq!(retry.changed_sessions[0].session.id, session_id);
    assert_eq!(retry.changed_sessions[0].session.preview, "newer version");

    let merged = merge_persist_delta_passes(deferred, retry);
    assert!(merged.deferred_session_ids.is_empty());
    assert_eq!(merged.changed_sessions.len(), 2);
    assert_eq!(
        merged
            .changed_sessions
            .iter()
            .filter(|record| record.session.id == stable_session_id)
            .count(),
        1,
        "the bounded retry must not duplicate the already-materialized stable row"
    );
    assert_eq!(
        merged
            .changed_sessions
            .iter()
            .find(|record| record.session.id == session_id)
            .expect("the retried session should be merged")
            .session
            .preview,
        "newer version"
    );
}

#[test]
fn persist_delta_plan_reemits_session_hide_and_removal_after_materialization() {
    let mut inner = StateInner::new();
    let hidden_id = inner
        .create_session(
            Agent::Claude,
            Some("Hidden after planning".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let removed_id = inner
        .create_session(
            Agent::Claude,
            Some("Removed after planning".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let stable_id = inner
        .create_session(
            Agent::Claude,
            Some("Stable during planning".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;

    let plan = inner.collect_persist_delta_plan(0);
    let selected_watermark = plan.watermark;
    let hidden_index = inner
        .find_session_index(&hidden_id)
        .expect("hidden candidate should exist");
    inner
        .session_mut_by_index(hidden_index)
        .expect("hidden candidate should be mutable")
        .hidden = true;
    let removed_index = inner
        .find_session_index(&removed_id)
        .expect("removed candidate should exist");
    inner.remove_session_at(removed_index);

    let first = inner.materialize_persist_delta(plan);
    assert_eq!(
        first
            .changed_sessions
            .iter()
            .map(|record| record.session.id.as_str())
            .collect::<Vec<_>>(),
        vec![stable_id.as_str()],
        "the stable row should materialize once while transitioned rows defer"
    );
    assert_eq!(
        first.deferred_session_ids,
        vec![hidden_id.clone(), removed_id.clone()]
    );

    let retry = inner.collect_persist_delta(first.watermark);
    assert!(retry.changed_sessions.is_empty());
    assert_eq!(retry.watermark, inner.last_mutation_stamp);
    assert!(retry.watermark > selected_watermark);
    assert!(retry.removed_session_ids.contains(&hidden_id));
    assert!(retry.removed_session_ids.contains(&removed_id));
    assert!(
        !retry
            .changed_sessions
            .iter()
            .any(|record| record.session.id == stable_id),
        "the stable row must not be selected again"
    );
}

#[test]
fn persist_delta_plan_reemits_delegation_change_and_removal_after_materialization() {
    let mut inner = StateInner::new();
    let parent_id = inner
        .create_session(
            Agent::Codex,
            Some("Parent".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let child_id = inner
        .create_session(
            Agent::Codex,
            Some("Child".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let changed_id = "delegation-changed-after-plan";
    let removed_id = "delegation-removed-after-plan";
    let stable_id = "delegation-stable-after-plan";
    for delegation_id in [changed_id, removed_id, stable_id] {
        inner.delegations.push(make_persist_delta_test_delegation(
            delegation_id,
            &parent_id,
            &child_id,
        ));
        inner.mark_delegation_id_mutated(delegation_id.to_owned());
    }

    let plan = inner.collect_persist_delta_plan(0);
    let selected_watermark = plan.watermark;
    let changed_index = inner
        .find_delegation_index(changed_id)
        .expect("changed delegation should exist");
    inner.delegations[changed_index].title = "Changed after planning".to_owned();
    inner.mark_delegation_mutated(changed_index);
    let removed_index = inner
        .find_delegation_index(removed_id)
        .expect("removed delegation should exist");
    inner.remove_delegation_at(removed_index);

    let first = inner.materialize_persist_delta(plan);
    assert_eq!(
        first
            .changed_delegations
            .as_ref()
            .expect("the stable delegation should materialize")
            .iter()
            .map(|delegation| delegation.id.as_str())
            .collect::<Vec<_>>(),
        vec![stable_id]
    );
    assert_eq!(
        first.deferred_delegation_ids,
        vec![changed_id.to_owned(), removed_id.to_owned()]
    );

    let retry = inner.collect_persist_delta(first.watermark);
    assert!(retry.watermark > selected_watermark);
    assert_eq!(
        retry
            .changed_delegations
            .as_ref()
            .expect("changed delegation should be retried")
            .iter()
            .map(|delegation| delegation.id.as_str())
            .collect::<Vec<_>>(),
        vec![changed_id]
    );
    assert_eq!(retry.removed_delegation_ids, vec![removed_id.to_owned()]);
}

#[test]
fn shared_persist_collection_replans_once_after_a_candidate_changes() {
    let mut inner = StateInner::new();
    let changing_id = inner
        .create_session(
            Agent::Claude,
            Some("Changing during shared collection".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let stable_id = inner
        .create_session(
            Agent::Claude,
            Some("Stable during shared collection".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let shared = StateMutex::new(inner);

    let delta = collect_persist_delta_from_shared_state_with_first_plan_hook(&shared, 0, || {
        let mut state = shared.lock().expect("state mutex poisoned");
        let changing_index = state
            .find_session_index(&changing_id)
            .expect("changing candidate should exist");
        state
            .session_mut_by_index(changing_index)
            .expect("changing candidate should be mutable")
            .session
            .preview = "newer shared version".to_owned();
    });

    assert!(delta.deferred_session_ids.is_empty());
    assert_eq!(delta.changed_sessions.len(), 2);
    assert_eq!(
        delta
            .changed_sessions
            .iter()
            .filter(|record| record.session.id == stable_id)
            .count(),
        1,
        "the retry pass must not select the stable session again"
    );
    assert_eq!(
        delta
            .changed_sessions
            .iter()
            .find(|record| record.session.id == changing_id)
            .expect("the newer candidate should materialize during the retry")
            .session
            .preview,
        "newer shared version"
    );
}

#[test]
fn shared_persist_collection_preserves_prompt_history_across_a_deferred_snapshot() {
    let mut inner = StateInner::new();
    let session_id = inner
        .create_session(
            Agent::Claude,
            Some("Prompt history deferred during shared collection".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let session_index = inner
        .find_session_index(&session_id)
        .expect("created session should exist");
    push_message_on_record(
        inner
            .session_mut_by_index(session_index)
            .expect("created session should be mutable"),
        Message::Text {
            attachments: Vec::new(),
            id: "deferred-user-prompt".to_owned(),
            timestamp: stamp_now(),
            author: Author::You,
            text: "persist this prompt".to_owned(),
            expanded_text: None,
            source: None,
        },
    );
    let shared = StateMutex::new(inner);

    let delta = collect_persist_delta_from_shared_state_with_first_plan_hook(&shared, 0, || {
        let mut state = shared.lock().expect("state mutex poisoned");
        let session_index = state
            .find_session_index(&session_id)
            .expect("created session should remain present");
        state
            .session_mut_by_index(session_index)
            .expect("created session should remain mutable")
            .session
            .preview = "changed after prompt-history selection".to_owned();
    });

    let persisted = delta
        .changed_sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("the bounded retry should materialize the changed session");
    assert!(
        persisted.persist_prompt_history,
        "the retry must retain prompt-history selection from the deferred first pass"
    );
    assert_eq!(
        serialize_persisted_session(persisted)
            .expect("the retried session should serialize")
            .prompt_history_value_json
            .as_deref(),
        Some("[\"persist this prompt\"]")
    );
}

#[test]
fn shared_persist_collection_carries_prompt_history_across_ticks_and_another_deferral() {
    let mut inner = StateInner::new();
    let session_id = inner
        .create_session(
            Agent::Claude,
            Some("Prompt history carried across ticks".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let session_index = inner
        .find_session_index(&session_id)
        .expect("created session should exist");
    push_message_on_record(
        inner
            .session_mut_by_index(session_index)
            .expect("created session should be mutable"),
        Message::Text {
            attachments: Vec::new(),
            id: "carried-user-prompt".to_owned(),
            timestamp: stamp_now(),
            author: Author::You,
            text: "carry this prompt".to_owned(),
            expanded_text: None,
            source: None,
        },
    );
    let watermark = inner.last_mutation_stamp;
    inner
        .session_mut_by_index(session_index)
        .expect("created session should remain mutable")
        .session
        .preview = "mutation after the previous tick watermark".to_owned();
    let shared = StateMutex::new(inner);
    let prompt_history_carry = BTreeSet::from([session_id.clone()]);

    let delta = collect_persist_delta_from_shared_state_with_carry_and_first_plan_hook(
        &shared,
        watermark,
        &prompt_history_carry,
        || {
            let mut state = shared.lock().expect("state mutex poisoned");
            let session_index = state
                .find_session_index(&session_id)
                .expect("created session should remain present");
            state
                .session_mut_by_index(session_index)
                .expect("created session should remain mutable")
                .session
                .preview = "mutation that defers the carried snapshot again".to_owned();
        },
    );

    let persisted = delta
        .changed_sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("the retry should materialize the twice-deferred session");
    assert!(persisted.persist_prompt_history);
    assert_eq!(
        serialize_persisted_session(persisted)
            .expect("the carried session should serialize")
            .prompt_history_value_json
            .as_deref(),
        Some("[\"carry this prompt\"]")
    );
}

#[test]
fn shared_persist_collection_releases_the_global_mutex_between_sessions() {
    let mut inner = StateInner::new();
    for name in ["First dirty session", "Second dirty session"] {
        let session_id = inner
            .create_session(
                Agent::Claude,
                Some(name.to_owned()),
                "/tmp".to_owned(),
                None,
                None,
            )
            .session
            .id;
        let session_index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        inner
            .session_mut_by_index(session_index)
            .expect("session should be mutable")
            .session
            .preview = format!("{name} changed");
    }

    let diagnostics = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&diagnostics);
    let reporter: StateMutexDiagnosticReporter = Arc::new(move |diagnostic| {
        captured
            .lock()
            .expect("diagnostic mutex poisoned")
            .push(diagnostic);
    });
    let shared = StateMutex::new_with_diagnostic_reporter(inner, Duration::ZERO, reporter);

    let delta = collect_persist_delta_from_shared_state(&shared, 0);
    assert_eq!(delta.changed_sessions.len(), 2);

    let held_count = diagnostics
        .lock()
        .expect("diagnostic mutex poisoned")
        .iter()
        .filter(|diagnostic| matches!(diagnostic, StateMutexDiagnostic::Held { .. }))
        .count();
    assert_eq!(
        held_count, 3,
        "collection should use one lightweight planning lock plus one lock per dirty session"
    );
}

#[test]
fn persist_delta_merge_prefers_a_later_session_removal_over_an_earlier_snapshot() {
    let mut inner = StateInner::new();
    let session_id = inner
        .create_session(
            Agent::Claude,
            Some("Removed after snapshot".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;

    let earlier = inner.collect_persist_delta(0);
    let session_index = inner
        .find_session_index(&session_id)
        .expect("snapshotted session should exist");
    inner
        .session_mut_by_index(session_index)
        .expect("snapshotted session should be mutable")
        .hidden = true;
    let later = inner.collect_persist_delta(earlier.watermark);

    let merged = merge_persist_delta_passes(earlier, later);
    assert!(
        !merged
            .changed_sessions
            .iter()
            .any(|record| record.session.id == session_id),
        "a later hidden-session tombstone must cancel the earlier row snapshot"
    );
    assert_eq!(merged.removed_session_ids, vec![session_id]);
}

#[test]
fn persist_delta_merge_prefers_later_delegation_state_in_both_directions() {
    let mut inner = StateInner::new();
    let parent_id = inner
        .create_session(
            Agent::Codex,
            Some("Parent".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let child_id = inner
        .create_session(
            Agent::Codex,
            Some("Child".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let delegation_id = "delegation-merge-order";
    inner.delegations.push(make_persist_delta_test_delegation(
        delegation_id,
        &parent_id,
        &child_id,
    ));
    inner.mark_delegation_id_mutated(delegation_id.to_owned());

    let snapshot = inner.collect_persist_delta(0);
    let delegation_index = inner
        .find_delegation_index(delegation_id)
        .expect("snapshotted delegation should exist");
    inner.remove_delegation_at(delegation_index);
    let removal = inner.collect_persist_delta(snapshot.watermark);

    let removed_merge = merge_persist_delta_passes(snapshot, removal);
    assert!(removed_merge.changed_delegations.is_none());
    assert_eq!(
        removed_merge.removed_delegation_ids,
        vec![delegation_id.to_owned()]
    );

    let mut replacement = make_persist_delta_test_delegation(delegation_id, &parent_id, &child_id);
    replacement.title = "Recreated after removal".to_owned();
    inner.delegations.push(replacement);
    inner.mark_delegation_id_mutated(delegation_id.to_owned());
    let recreated = inner.collect_persist_delta(removed_merge.watermark);

    let recreated_merge = merge_persist_delta_passes(removed_merge, recreated);
    assert!(recreated_merge.removed_delegation_ids.is_empty());
    let changed = recreated_merge
        .changed_delegations
        .expect("the later recreated delegation should replace the removal");
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].id, delegation_id);
    assert_eq!(changed[0].title, "Recreated after removal");
}
