//! Refresh contract against the pre-optimization lifecycle transaction.
use super::delegation_support::{
    finish_delegation_child_with_assistant_text,
    test_app_state_with_drained_delegation_codex_runtime,
};
use super::*;

struct RefreshFixture {
    state: AppState,
    child: String,
    substitutions: Vec<(String, String)>,
}

fn enable_fixture_mailbox(state: &mut AppState) {
    let path = resolve_coordination_persistence_path(state.persistence_path.as_ref());
    state.mailbox_store = Arc::new(MailboxStore::open(&path).unwrap());
    assert!(state.mailbox_store.connection_if_enabled().is_some());
}

fn fixture(outcome: &str, mode: DelegationWaitMode) -> RefreshFixture {
    let state = test_app_state_with_drained_delegation_codex_runtime("refresh-cut-fixture");
    let parent = test_session_id(&state, Agent::Codex);
    let mut children = Vec::new();
    let mut substitutions = vec![
        (state.server_instance_id.clone(), "$instance".to_owned()),
        (state.default_workdir.clone(), "$workdir".to_owned()),
    ];
    for index in 0..2 {
        let created = state.create_read_only_delegation(&parent, CreateDelegationRequest {
            prompt: "Inspect refresh fixture.".to_owned(), title: Some(format!("Child {index}")),
            cwd: None, agent: Some(Agent::Codex), model: None,
            mode: Some(DelegationMode::Explorer), write_policy: Some(DelegationWritePolicy::ReadOnly),
        }).unwrap();
        // The generated prompt contains a UUID; its truncated preview cannot
        // be normalized by replacing the complete UUID after capture.
        {
            let mut inner = state.inner.lock().unwrap();
            let child_index = inner.find_session_index(&created.delegation.child_session_id).unwrap();
            inner.session_mut_by_index(child_index).unwrap().session.preview =
                format!("Child {index} fixture preview");
        }
        substitutions.push((created.delegation.id.clone(), format!("$delegation{index}")));
        children.push(created);
    }
    {
        let mut inner = state.inner.lock().unwrap();
        let parent_index = inner.find_session_index(&parent).unwrap();
        inner.session_mut_by_index(parent_index).unwrap().session.status = SessionStatus::Active;
        inner.delegation_waits.push(DelegationWaitRecord {
            id: "wait-related".to_owned(), parent_session_id: parent.clone(),
            delegation_ids: children.iter().map(|child| child.delegation.id.clone()).collect(),
            mode, created_at: "2026-01-01T00:00:00Z".to_owned(), title: None,
        });
        inner.delegation_waits.push(DelegationWaitRecord {
            id: "wait-unrelated".to_owned(), parent_session_id: parent,
            delegation_ids: vec![children[1].delegation.id.clone()],
            mode: DelegationWaitMode::All, created_at: "2026-01-01T00:00:00Z".to_owned(), title: None,
        });
        state.commit_locked(&mut inner).unwrap();
    }
    let child = children[0].delegation.child_session_id.clone();
    match outcome {
        "completed" => finish_delegation_child_with_assistant_text(
            &state, &child, "## Result\n\nStatus: completed\n\nSummary:\nRefresh complete.",
        ),
        "failed" => {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&child).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            record.session.status = SessionStatus::Error;
            record.session.preview = "fixture failure".to_owned();
            state.commit_locked(&mut inner).unwrap();
        }
        "running" => (),
        _ => panic!("unknown fixture outcome"),
    }
    {
        let inner = state.inner.lock().unwrap();
        for record in &inner.sessions {
            substitutions.push((record.federation_native_creation_id.clone(),
                format!("$creation-{}", record.session.id)));
        }
    }
    RefreshFixture { state, child, substitutions }
}

// Frozen transaction from refresh_delegation_for_child_session at 05b442b3.
// This is the old reference path, deliberately retaining global wait/commit
// behavior. The candidate is exercised through the real public owner below.
fn reference_refresh(state: &AppState, child: &str) {
    state.recover_durable_delegation_review_submission(child).unwrap();
    let (revision, lifecycle, detached, waits) = {
        let mut inner = state.inner.lock().unwrap();
        let Some(index) = inner.find_delegation_index_by_child_session_id(child) else { return };
        let Some(lifecycle) = refresh_delegation_from_child_locked(&mut inner, index) else { return };
        let detached = detach_terminal_delegation_child_runtime_locked(&mut inner, index);
        let waits = refresh_delegation_waits_locked(&mut inner);
        let revision = state.commit_locked(&mut inner).unwrap();
        state.enqueue_delegation_refresh_locked(&inner, revision, Some(&lifecycle), &detached, &waits);
        (revision, lifecycle, detached, waits)
    };
    state.publish_delegation_refresh_side_effects(revision, Some(lifecycle), detached, waits);
}

fn normalize(mut value: serde_json::Value, substitutions: &[(String, String)]) -> serde_json::Value {
    fn times(value: &serde_json::Value, found: &mut BTreeSet<String>) {
        match value {
            serde_json::Value::String(text) if chrono::DateTime::parse_from_rfc3339(text).is_ok() => { found.insert(text.clone()); }
            serde_json::Value::Array(rows) => rows.iter().for_each(|row| times(row, found)),
            serde_json::Value::Object(fields) => {
                for (key, row) in fields {
                    if matches!(key.as_str(), "timestamp" | "completedAt" | "failedAt") {
                        if let Some(text) = row.as_str() {
                            if chrono::NaiveTime::parse_from_str(text, "%H:%M:%S").is_ok() {
                                found.insert(text.to_owned());
                            }
                        }
                    }
                    times(row, found);
                }
            }
            _ => (),
        }
    }
    fn replace(value: &mut serde_json::Value, substitutions: &[(String, String)]) {
        match value {
            serde_json::Value::String(text) => {
                for (from, to) in substitutions { *text = text.replace(from, to); }
            }
            serde_json::Value::Array(rows) => rows.iter_mut().for_each(|row| replace(row, substitutions)),
            serde_json::Value::Object(fields) => {
                let previous = std::mem::take(fields);
                for (mut key, mut row) in previous {
                    for (from, to) in substitutions { key = key.replace(from, to); }
                    replace(&mut row, substitutions);
                    fields.insert(key, row);
                }
            }
            _ => (),
        }
    }
    // Independent fixture clocks/UUIDs differ. Keep every semantic field,
    // sequence, revision, wait id, status, prompt and mutation stamp.
    let mut timestamps = BTreeSet::new();
    times(&value, &mut timestamps);
    let mut substitutions = substitutions.to_vec();
    substitutions.extend(timestamps.into_iter().map(|time| (time, "$time".to_owned())));
    replace(&mut value, &substitutions);
    value
}

fn image(fixture: &RefreshFixture, events: &mut broadcast::Receiver<String>) -> serde_json::Value {
    let inner = fixture.state.inner.lock().unwrap();
    let memory = serde_json::to_value(PersistedState::from_inner(&inner)).unwrap();
    let runtime_present = inner.sessions.iter().map(|session| {
        (session.session.id.clone(), !matches!(session.runtime, SessionRuntime::None))
    }).collect::<BTreeMap<_, _>>();
    drop(inner);
    // This opens ONLY the disposable fixture's database, never a live store.
    let disk = load_state(&fixture.state.persistence_path).unwrap().unwrap();
    // Each disposable database acquires its own identity on load, independently
    // of the delegation transaction under comparison.
    let mut substitutions = fixture.substitutions.clone();
    substitutions.push((disk.federation.local.as_ref().unwrap().host_id.clone(), "$fixture-host".to_owned()));
    let disk = serde_json::to_value(PersistedState::from_inner(&disk)).unwrap();
    let mut deltas = Vec::<serde_json::Value>::new();
    while let Ok(payload) = events.try_recv() { deltas.push(serde_json::from_str(&payload).unwrap()); }
    normalize(json!({"memory": memory, "disk": disk, "deltas": deltas, "runtimePresent": runtime_present}), &substitutions)
}

#[test]
fn delegation_refresh_equivalence_preserves_lifecycle_waits_runtime_and_persistence() {
    for mode in [DelegationWaitMode::Any, DelegationWaitMode::All] {
        for outcome in ["completed", "failed", "running"] {
            let old = fixture(outcome, mode);
            let candidate = fixture(outcome, mode);
            let mut old_events = old.state.subscribe_delta_events();
            let mut candidate_events = candidate.state.subscribe_delta_events();
            reference_refresh(&old.state, &old.child);
            candidate.state.refresh_delegation_for_child_session(&candidate.child).unwrap();
            assert_eq!(image(&candidate, &mut candidate_events), image(&old, &mut old_events), "{outcome}, {mode:?}");
        }
    }
}

#[test]
fn delegation_refresh_does_not_capture_the_global_state() {
    let fixture = fixture("completed", DelegationWaitMode::Any);
    let mut stream = fixture.state.subscribe_stream_events();
    fixture.state.refresh_delegation_for_child_session(&fixture.child).unwrap();
    let mut delta_count = 0;
    while let Ok(event) = stream.try_recv() {
        assert!(matches!(event, SseStreamEvent::Delta(_)), "child refresh must publish only its existing deltas");
        delta_count += 1;
    }
    assert!(delta_count > 0, "the lifecycle change must still publish");
}

#[test]
fn delegation_refresh_does_not_scan_delegations_for_child_lookup() {
    let mut fixture = fixture("completed", DelegationWaitMode::All);
    enable_fixture_mailbox(&mut fixture.state);
    TEST_DELEGATION_CHILD_SCAN_COUNT.with(|count| count.set(0));
    fixture.state.refresh_delegation_for_child_session(&fixture.child).unwrap();
    assert_eq!(TEST_DELEGATION_CHILD_SCAN_COUNT.with(|count| count.get()), 0, "use the existing delegation index owner");
}

#[test]
fn delegation_refresh_unknown_child_is_a_noop_without_an_inventory_scan() {
    let mut state = super::test_app_state();
    enable_fixture_mailbox(&mut state);
    let before = state.inner.lock().unwrap().revision;
    let mut events = state.subscribe_stream_events();
    TEST_DELEGATION_CHILD_SCAN_COUNT.with(|count| count.set(0));
    state.refresh_delegation_for_child_session("session-not-a-child").unwrap();
    assert_eq!(state.inner.lock().unwrap().revision, before);
    assert!(matches!(events.try_recv(), Err(broadcast::error::TryRecvError::Empty)));
    assert_eq!(TEST_DELEGATION_CHILD_SCAN_COUNT.with(|count| count.get()), 0);
}

#[test]
fn delegation_refresh_boot_caller_matches_the_original_terminal_transaction() {
    let old = fixture("completed", DelegationWaitMode::Any);
    let candidate = fixture("completed", DelegationWaitMode::Any);
    for fixture in [&old, &candidate] {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_delegation_index_by_child_session_id(&fixture.child).unwrap();
        inner.delegations[index].mode = DelegationMode::Reviewer;
        inner.delegations[index].review_result_schema_version = None;
        inner.delegations[index].submitted_review_result = None;
        inner.mark_delegation_mutated(index);
        fixture.state.commit_locked(&mut inner).unwrap();
    }
    let mut old_events = old.state.subscribe_delta_events();
    let mut candidate_events = candidate.state.subscribe_delta_events();
    reference_refresh(&old.state, &old.child);
    candidate.state.reconcile_durable_delegation_review_submissions_after_boot();
    assert_eq!(image(&candidate, &mut candidate_events), image(&old, &mut old_events));
}

#[test]
fn delegation_refresh_enabled_mailbox_recovery_miss_uses_the_index() {
    let mut fixture = fixture("completed", DelegationWaitMode::Any);
    enable_fixture_mailbox(&mut fixture.state);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        inner.delegations[0].mode = DelegationMode::Reviewer;
        inner.mark_delegation_mutated(0);
    }
    TEST_DELEGATION_CHILD_SCAN_COUNT.with(|count| count.set(0));
    fixture.state.refresh_delegation_for_child_session(&fixture.child).unwrap();
    let inner = fixture.state.inner.lock().unwrap();
    let record = &inner.delegations[0];
    assert_eq!(record.review_result_recovery_probe_attempt, Some(record.review_result_submission_attempt));
    assert_eq!(record.status, DelegationStatus::Failed);
    assert_eq!(TEST_DELEGATION_CHILD_SCAN_COUNT.with(|count| count.get()), 0);
}

#[test]
fn delegation_refresh_enabled_mailbox_restores_the_result_without_scanning() {
    let mut fixture = fixture("completed", DelegationWaitMode::Any);
    enable_fixture_mailbox(&mut fixture.state);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        inner.delegations[0].mode = DelegationMode::Reviewer;
        inner.mark_delegation_mutated(0);
    }
    fixture.state.submit_delegation_review_result(
        &fixture.child, super::delegation_support::structured_review_request(),
    ).unwrap();
    // Keep the real committed mailbox envelope, but simulate the primary row
    // from before its installation. Recovery must revalidate the same attempt.
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let record = &mut inner.delegations[0];
        record.status = DelegationStatus::Running;
        record.result = None;
        record.submitted_review_result = None;
        record.review_result_schema_version = None;
        record.review_result_recovery_probe_attempt = None;
        inner.mark_delegation_mutated(0);
    }
    TEST_DELEGATION_CHILD_SCAN_COUNT.with(|count| count.set(0));
    fixture.state.refresh_delegation_for_child_session(&fixture.child).unwrap();
    let inner = fixture.state.inner.lock().unwrap();
    let record = &inner.delegations[0];
    assert_eq!(record.status, DelegationStatus::Completed);
    assert_eq!(record.review_result_schema_version, Some(DELEGATION_REVIEW_RESULT_SCHEMA_VERSION));
    assert_eq!(record.result.as_ref().unwrap().summary, "One medium issue found.");
    assert!(record.submitted_review_result.is_none(), "terminalization consumes the staged submission");
    assert_eq!(TEST_DELEGATION_CHILD_SCAN_COUNT.with(|count| count.get()), 0);
}

#[test]
fn delegation_refresh_model_interaction_delta_precedes_terminal_refresh_deltas() {
    let fixture = fixture("completed", DelegationWaitMode::All);
    let message_id = fixture.state.last_message_id(&fixture.child).unwrap().unwrap();
    let mut events = fixture.state.subscribe_delta_events();
    fixture.state.commit_interaction_message_update(&fixture.child, &message_id, |record| {
        record.session.messages.iter().position(|message| message.id() == message_id)
            .ok_or_else(|| ApiError::not_found("fixture message missing"))
    }).unwrap();
    let mut deltas = Vec::<serde_json::Value>::new();
    while let Ok(payload) = events.try_recv() { deltas.push(serde_json::from_str(&payload).unwrap()); }
    assert_eq!(deltas[0]["type"], "messageUpdated");
    assert_eq!(deltas[0]["sessionId"], fixture.child);
    let lifecycle = deltas.iter().find(|delta| delta["type"] == "delegationCompleted")
        .expect("model response must refresh the delegation");
    assert!(lifecycle["revision"].as_u64().unwrap() > deltas[0]["revision"].as_u64().unwrap());
    let mut inner = fixture.state.inner.lock().unwrap();
    let delta = inner.collect_persist_delta(0);
    assert!(delta.changed_delegations.as_ref().unwrap().iter()
        .any(|record| record.child_session_id == fixture.child && record.status == DelegationStatus::Completed));
}

#[test]
fn delegation_refresh_index_tracks_relationship_changes_and_shifted_rows() {
    let fixture = fixture("running", DelegationWaitMode::All);
    let mut inner = fixture.state.inner.lock().unwrap();
    let first = inner.indexed_delegation_for_child(&fixture.child).unwrap();
    let second_child = inner.delegations[1].child_session_id.clone();
    assert_eq!(inner.indexed_delegation_for_child(&second_child), Some(1));
    inner.delegations[first].child_session_id = "session-reassigned".to_owned();
    inner.mark_delegation_mutated(first);
    assert_eq!(inner.indexed_delegation_for_child(&fixture.child), None);
    assert_eq!(inner.indexed_delegation_for_child("session-reassigned"), Some(first));
    inner.remove_delegation_at(first);
    assert_eq!(inner.indexed_delegation_for_child(&second_child), Some(0));
    assert_eq!(inner.indexed_delegation_for_child("session-reassigned"), None);
}

#[test]
fn delegation_refresh_does_not_consume_an_unrelated_ready_wait() {
    let fixture = fixture("completed", DelegationWaitMode::Any);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        // A separately completed sibling is the global refresher's work. This
        // child's refresh must not consume a wait watching only that sibling.
        inner.delegations[1].status = DelegationStatus::Completed;
        inner.mark_delegation_mutated(1);
        fixture.state.commit_locked(&mut inner).unwrap();
    }
    fixture.state.refresh_delegation_for_child_session(&fixture.child).unwrap();
    let inner = fixture.state.inner.lock().unwrap();
    assert!(inner.delegation_waits.iter().any(|wait| wait.id == "wait-unrelated"));
    assert!(!inner.delegation_waits.iter().any(|wait| wait.id == "wait-related"));
}

#[test]
fn delegation_refresh_stays_between_concurrent_locked_commits() {
    let fixture = fixture("completed", DelegationWaitMode::All);
    let mut events = fixture.state.subscribe_delta_events();
    let state = fixture.state.clone();
    let child = fixture.child.clone();
    let mut inner = fixture.state.inner.lock().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        ready_tx.send(()).unwrap();
        state.refresh_delegation_for_child_session(&child).unwrap();
    });
    recv_within_guard(&ready_rx, "refresh thread ready").unwrap();
    let before = fixture.state.commit_persisted_delta_locked(&mut inner).unwrap();
    fixture.state.publish_delta_locked(&inner, DeltaEvent::CodexUpdated {
        revision: before, codex: inner.codex.clone(),
    });
    drop(inner);
    worker.join().unwrap();
    let mut inner = fixture.state.inner.lock().unwrap();
    let after = fixture.state.commit_persisted_delta_locked(&mut inner).unwrap();
    fixture.state.publish_delta_locked(&inner, DeltaEvent::CodexUpdated {
        revision: after, codex: inner.codex.clone(),
    });
    drop(inner);
    let mut revisions = Vec::new();
    while let Ok(payload) = events.try_recv() {
        let event: DeltaEvent = serde_json::from_str(&payload).unwrap();
        revisions.push(event.revision());
    }
    assert_eq!(revisions.first(), Some(&before));
    assert_eq!(revisions.last(), Some(&after));
    assert!(revisions.iter().any(|revision| *revision > before && *revision < after));
    assert!(revisions.windows(2).all(|pair| pair[0] <= pair[1]));
}
