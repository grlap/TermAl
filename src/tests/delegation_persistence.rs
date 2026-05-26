//! Delegation persistence-delta tests split from `delegations.rs`.
//!
//! This module owns delegation row persistence, tombstone, and migration delta
//! coverage. It deliberately does not own wait fan-in, read-only enforcement,
//! or result recovery tests.

use super::delegation_support::{
    finish_delegation_child_with_assistant_text,
    test_app_state_with_drained_delegation_codex_runtime,
};
use super::*;

fn test_app_state() -> AppState {
    test_app_state_with_drained_delegation_codex_runtime("delegation-persistence-runtime")
}

fn create_delegation_for_persist_transition(title: &str) -> (AppState, DelegationResponse, u64) {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: format!("Check {title} persistence transition coverage."),
                title: Some(title.to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let watermark = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.collect_persist_delta(0).watermark
    };
    (state, created, watermark)
}

fn assert_persist_delta_contains_delegation_status(
    delta: &PersistDelta,
    delegation_id: &str,
    status: DelegationStatus,
) {
    let delegations = delta
        .changed_delegations
        .as_ref()
        .expect("delegation transition should be persisted");
    let delegation = delegations
        .iter()
        .find(|delegation| delegation.id == delegation_id)
        .expect("updated delegation should be present");
    assert_eq!(delegation.status, status);
}

#[test]
fn delegation_create_and_completion_are_included_in_persist_delta() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let before_create = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner.last_mutation_stamp
    };

    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Check persistence delta coverage.".to_owned(),
                title: Some("Persist Delta".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let create_watermark = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let delta = inner.collect_persist_delta(before_create);
        let delegations = delta
            .changed_delegations
            .as_ref()
            .expect("created delegation should be persisted");
        assert!(
            delegations
                .iter()
                .any(|delegation| delegation.id == created.delegation.id)
        );
        delta.watermark
    };
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        assert!(
            inner
                .collect_persist_delta(create_watermark)
                .changed_delegations
                .is_none(),
            "delegations should not be rewritten again after the watermark catches up"
        );
    }

    let before_completion = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner.last_mutation_stamp
    };
    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nDelegation complete.",
    );
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete delegation");

    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let delta = inner.collect_persist_delta(before_completion);
    let delegations = delta
        .changed_delegations
        .as_ref()
        .expect("completed delegation should be persisted");
    let delegation = delegations
        .iter()
        .find(|delegation| delegation.id == created.delegation.id)
        .expect("completed delegation should be present");
    assert_eq!(delegation.status, DelegationStatus::Completed);
}

#[test]
fn sqlite_empty_delegation_table_preserves_embedded_delegations_for_migration() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Persisted before dedicated delegation table.".to_owned(),
                title: Some("Embedded Delegation".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let mut persisted = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        PersistedState::from_inner(&inner)
    };

    let loaded_from_table = apply_sqlite_delegation_records(&mut persisted, Vec::new());

    assert!(!loaded_from_table);
    assert!(
        persisted
            .delegations
            .iter()
            .any(|delegation| delegation.id == created.delegation.id),
        "empty dedicated table must not wipe embedded delegation metadata"
    );

    let mut inner = persisted
        .into_inner()
        .expect("embedded delegation state should load");
    inner.mark_loaded_delegations_for_sqlite_migration();
    let delta = inner.collect_persist_delta(0);
    let delegations = delta
        .changed_delegations
        .as_ref()
        .expect("embedded delegations should be migrated into the dedicated table");
    assert!(
        delegations
            .iter()
            .any(|delegation| delegation.id == created.delegation.id)
    );
}

#[test]
fn pure_delegation_update_is_included_in_persist_delta() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Check pure delegation persistence delta coverage.".to_owned(),
                title: Some("Initial Delegation Title".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let create_watermark = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.collect_persist_delta(0).watermark
    };

    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter_mut()
        .find(|delegation| delegation.id == created.delegation.id)
        .expect("delegation should exist");
    delegation.title = "Pure Delegation Update".to_owned();
    let delegation_index = inner
        .find_delegation_index(&created.delegation.id)
        .expect("delegation should exist");
    inner.mark_delegation_mutated(delegation_index);

    let delta = inner.collect_persist_delta(create_watermark);
    assert!(
        delta.changed_sessions.is_empty(),
        "pure delegation updates should not depend on a restamped session"
    );
    let delegations = delta
        .changed_delegations
        .as_ref()
        .expect("pure delegation update should be persisted");
    assert_eq!(delegations.len(), 1);
    let delegation = delegations
        .iter()
        .find(|delegation| delegation.id == created.delegation.id)
        .expect("updated delegation should be present");
    assert_eq!(delegation.title, "Pure Delegation Update");
}

#[test]
fn delegation_persist_delta_includes_only_changed_delegation_rows() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let first = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "First unchanged delegation.".to_owned(),
                title: Some("Unchanged Delegation".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("first delegation should be created");
    let second = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Second changed delegation.".to_owned(),
                title: Some("Changed Delegation".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("second delegation should be created");
    let watermark = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.collect_persist_delta(0).watermark
    };

    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let delegation_index = inner
        .find_delegation_index(&second.delegation.id)
        .expect("second delegation should exist");
    inner.delegations[delegation_index].title = "Only This Row Changed".to_owned();
    inner.mark_delegation_mutated(delegation_index);

    let delta = inner.collect_persist_delta(watermark);
    let delegations = delta
        .changed_delegations
        .as_ref()
        .expect("changed delegation should be persisted");
    assert_eq!(
        delegations
            .iter()
            .map(|delegation| delegation.id.as_str())
            .collect::<Vec<_>>(),
        vec![second.delegation.id.as_str()]
    );
    assert!(
        !delegations
            .iter()
            .any(|delegation| delegation.id == first.delegation.id),
        "unchanged delegation rows should not be rewritten"
    );
}

#[test]
fn delegation_persist_delta_includes_removed_delegation_tombstones() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Delegation to remove from persistence.".to_owned(),
                title: Some("Removed Delegation".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let watermark = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.collect_persist_delta(0).watermark
    };

    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let delegation_index = inner
        .find_delegation_index(&created.delegation.id)
        .expect("delegation should exist");
    inner.remove_delegation_at(delegation_index);

    let delta = inner.collect_persist_delta(watermark);
    assert_eq!(
        delta.removed_delegation_ids,
        vec![created.delegation.id.clone()]
    );
    assert!(inner.removed_delegation_ids.is_empty());
    assert!(
        delta.changed_delegations.is_none(),
        "removed delegation rows should not be re-upserted"
    );
    let drained_tombstones = delta.drained_delegation_tombstones.clone();

    inner.restore_drained_delegation_tombstones(&drained_tombstones);
    let retry_delta = inner.collect_persist_delta(watermark);
    assert_eq!(
        retry_delta.removed_delegation_ids,
        vec![created.delegation.id.clone()]
    );
}

#[test]
fn delegation_status_transitions_are_included_in_persist_delta() {
    let (state, created, watermark) = create_delegation_for_persist_transition("Running Delta");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let delegation_index = inner
            .find_delegation_index(&created.delegation.id)
            .expect("delegation should exist");
        inner.delegations[delegation_index].status = DelegationStatus::Queued;
        let lifecycle_delta = refresh_delegation_from_child_locked(&mut inner, delegation_index)
            .expect("queued delegation with active child should transition to running");
        assert!(matches!(
            lifecycle_delta,
            DelegationLifecycleDelta::Updated {
                status: DelegationStatus::Running,
                ..
            }
        ));
        let delta = inner.collect_persist_delta(watermark);
        assert_persist_delta_contains_delegation_status(
            &delta,
            &created.delegation.id,
            DelegationStatus::Running,
        );
    }

    let (state, created, watermark) = create_delegation_for_persist_transition("Failed Delta");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let delegation_index = inner
            .find_delegation_index(&created.delegation.id)
            .expect("delegation should exist");
        mark_delegation_failed_locked(&mut inner, delegation_index, "transition failed")
            .expect("running delegation should transition to failed");
        let delta = inner.collect_persist_delta(watermark);
        assert_persist_delta_contains_delegation_status(
            &delta,
            &created.delegation.id,
            DelegationStatus::Failed,
        );
    }

    let (state, created, watermark) = create_delegation_for_persist_transition("Canceled Delta");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let delegation_index = inner
            .find_delegation_index(&created.delegation.id)
            .expect("delegation should exist");
        mark_delegation_canceled_locked(
            &mut inner,
            delegation_index,
            Some("transition canceled".to_owned()),
        )
        .expect("running delegation should transition to canceled");
        let delta = inner.collect_persist_delta(watermark);
        assert_persist_delta_contains_delegation_status(
            &delta,
            &created.delegation.id,
            DelegationStatus::Canceled,
        );
    }
}
