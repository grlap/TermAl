//! Session-local transcript rejection must not fail a sibling admission fence.
use super::*;

#[path = "persist_queue_promotion_conflict.rs"]
mod queue_promotion;

fn message(id: &str) -> Message {
    Message::Text {
        id: id.to_owned(),
        timestamp: "2026-11-01T00:00:00Z".into(),
        author: Author::Assistant,
        text: format!("retained {id}"),
        expanded_text: None,
        source: None,
        attachments: Vec::new(),
    }
}

/// Another message under an already durable id: a remote proxy still refuses
/// it, while the same message relocated would move to its new position.
fn other_message(id: &str) -> Message {
    Message::Text {
        id: id.to_owned(),
        timestamp: "2026-11-01T00:00:00Z".into(),
        author: Author::You,
        text: format!("another message under {id}"),
        expanded_text: None,
        source: None,
        attachments: Vec::new(),
    }
}

fn conflict_does_not_fail_sibling_admission(sparse: bool) {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let mut fixture = FenceFixture::new();
    let (bad, good) = {
        let mut inner = fixture.inner.lock().unwrap();
        let cwd = fixture.root.path().to_string_lossy().into_owned();
        let bad = inner.create_session(Agent::Codex, None, cwd.clone(), None, None).session.id;
        let good = inner.create_session(Agent::Codex, None, cwd, None, None).session.id;
        let index = inner.find_session_index(&bad).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        for id in ["body-0", "body-1", "body-2"] {
            push_message_on_record(record, message(id));
        }
        (bad, good)
    };
    fixture.write(&fixture.collect()).unwrap();
    let stored = |connection: &rusqlite::Connection| -> (String, Vec<(i64, String, String)>, Vec<u8>, Option<String>) {
        let metadata = connection.query_row(
            "SELECT value_json FROM sessions WHERE id=?1", [&bad], |row| row.get(0),
        ).unwrap();
        let rows = connection.prepare(
            "SELECT position,message_id,value_json FROM messages WHERE session_id=?1 ORDER BY position",
        ).unwrap().query_map([&bad], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
        let overview = connection.query_row(
            "SELECT value_blob FROM session_overviews WHERE session_id=?1", [&bad], |row| row.get(0),
        ).unwrap();
        let history = connection.query_row(
            "SELECT value_json FROM session_prompt_histories WHERE session_id=?1", [&bad], |row| row.get(0),
        ).optional().unwrap();
        (metadata, rows, overview, history)
    };
    let before = stored(fixture.cache.connection.as_ref().unwrap());
    let previous_baseline = &fixture.cache.transcripts[&bad];
    let previous_start = previous_baseline.start;
    let previous_identities = previous_baseline.identities.clone();
    let (bad_target, good_target) = {
        let mut inner = fixture.inner.lock().unwrap();
        let index = inner.find_session_index(&bad).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.engram.dispatch_generation = 2;
        record.engram.routing_token = Some("rejected-new-owner".into());
        record.session.prompt_history.push("unsaved history".into());
        record.prompt_history_mutation_stamp = record.mutation_stamp;
        if sparse {
            record.session.messages.remove(0);
            record.session.messages.remove(0);
            record.message_start_index = 2;
            record.session.messages_loaded = false;
            record.session.messages[0] = message("body-0");
        } else {
            push_message_on_record(record, message("body-0"));
        }
        record.message_positions = build_message_positions(&record.session.messages);
        let bad_target = PersistFenceTarget::EngramAdmission {
            session_id: bad.clone(),
            content: engram_admission_persisted_content(&PersistedSessionRecord::from_record(record)),
        };
        let index = inner.find_session_index(&good).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.engram.dispatch_generation = 42;
        record.engram.routing_token = Some("healthy-new-owner".into());
        let good_target = PersistFenceTarget::EngramAdmission {
            session_id: good.clone(),
            content: engram_admission_persisted_content(&PersistedSessionRecord::from_record(record)),
        };
        (bad_target, good_target)
    };
    let bad_waiter = fixture.enqueue(bad_target.clone());
    let good_waiter = fixture.enqueue(good_target.clone());
    fixture.receive(false);
    fixture.batch.drain(&fixture.rx, false);
    assert_eq!(fixture.batch.pending.len(), 2, "both admissions must share the failing write");
    assert_eq!(poll(&bad_waiter), None);
    assert_eq!(poll(&good_waiter), None);
    let delta = fixture.collect();
    let observer = Arc::new(PersistObserver::default());
    let observation = observer.begin_at(std::time::Instant::now());
    let result = fixture.write(&delta);
    observation.finish(result.is_ok(), &PersistTickTimings::default());
    let diagnostic = observer.snapshot_at(std::time::Instant::now());
    assert_eq!(diagnostic.rejected_session_writes, 1);
    let rejected = diagnostic.last_session_rejection.unwrap();
    assert_eq!(rejected.session_id, bad);
    if sparse {
        assert!(rejected.error.contains("UNIQUE constraint failed: messages.session_id, messages.message_id"));
        assert!(rejected.error.contains("position 2"));
    } else {
        assert!(rejected.error.contains("duplicate transcript identity"));
    }
    assert_eq!(poll(&good_waiter), Some(Ok(())),
        "a conflicting sibling transcript must not make unrelated admission durability unknown: {result:?}");
    let persisted = result.expect("healthy sibling must commit despite transcript rejection");
    assert!(persisted.contains(&good));
    assert!(!persisted.contains(&bad), "rejected session cannot be reported durable");
    assert_ne!(poll(&bad_waiter), Some(Ok(())), "rejected admission must not receive a false ACK");
    let connection = fixture.cache.connection.as_ref().unwrap();
    assert!(good_target.is_already_durable(connection).unwrap());
    assert!(!bad_target.is_already_durable(connection).unwrap());
    assert_eq!(stored(connection), before, "keep rejected metadata and transcript at the last good cut");
    let baseline = &fixture.cache.transcripts[&bad];
    assert_eq!(baseline.start, previous_start);
    assert_eq!(baseline.identities.len(), previous_identities.len());
    assert!(baseline.identities.iter().zip(&previous_identities)
        .all(|(current, previous)| current.ptr_eq(previous)),
        "keep the last-good baseline without adopting rejected snapshot identities");
    retain_unpersisted_session_changes(&fixture.inner, &delta, &persisted);
    let retry = collect_persist_delta_from_shared_state(&fixture.inner, delta.watermark);
    assert_eq!(retry.changed_sessions.len(), 1, "global progress must retain only the rejected row's retry duty");
    assert_eq!(retry.changed_sessions[0].session.id, bad);
    assert!(retry.changed_sessions[0].persist_prompt_history, "rejected prompt history must also remain dirty");
    // A later legitimate producer correction, not a database repair, can
    // discharge the original exact admission fence without another request.
    {
        let mut inner = fixture.inner.lock().unwrap();
        let record = inner.session_mut(&bad).unwrap();
        if sparse {
            record.session.messages[0] = message("body-2");
        } else {
            record.session.messages[3] = message("body-3");
        }
        record.message_positions = build_message_positions(&record.session.messages);
    }
    let recovered = collect_persist_delta_from_shared_state(&fixture.inner, delta.watermark);
    assert_eq!(fixture.write(&recovered).unwrap(), vec![bad.clone()]);
    assert_eq!(poll(&bad_waiter), Some(Ok(())));
    assert!(bad_target.is_already_durable(fixture.cache.connection.as_ref().unwrap()).unwrap());
}

#[test]
fn duplicate_incoming_id_does_not_poison_sibling_admission_fence() {
    conflict_does_not_fail_sibling_admission(false);
}

#[test]
fn sparse_id_collision_does_not_poison_sibling_admission_fence() {
    conflict_does_not_fail_sibling_admission(true);
}

fn unrelated_constraint_is_fatal(trigger_body: &str) {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let mut fixture = FenceFixture::new();
    let id = {
        let mut inner = fixture.inner.lock().unwrap();
        inner.create_session(Agent::Codex, None, fixture.root.path().to_string_lossy().into_owned(), None, None).session.id
    };
    fixture.write(&fixture.collect()).unwrap();
    fixture.cache.connection.as_ref().unwrap().execute_batch(&format!(
        "CREATE TABLE unrelated_unique(id INTEGER UNIQUE); INSERT INTO unrelated_unique VALUES(1);
         CREATE TEMP TRIGGER reject_transcript BEFORE INSERT ON main.messages BEGIN {trigger_body} END;"
    )).unwrap();
    let target = {
        let mut inner = fixture.inner.lock().unwrap();
        let record = inner.session_mut(&id).unwrap();
        record.engram.dispatch_generation = 42;
        record.engram.routing_token = Some("must-not-ack".into());
        push_message_on_record(record, message("trigger-message"));
        PersistFenceTarget::EngramAdmission {
            session_id: id,
            content: engram_admission_persisted_content(&PersistedSessionRecord::from_record(record)),
        }
    };
    let waiter = fixture.enqueue(target.clone());
    fixture.receive(false);
    let error = fixture.write(&fixture.collect()).unwrap_err();
    assert!(format!("{error:#}").contains("constraint") || format!("{error:#}").contains("UNIQUE"));
    assert!(matches!(poll(&waiter), Some(Err(PersistFenceError::WriteFailed(_)))));
    let stored = rusqlite::Connection::open(fixture.root.path().join("termal.sqlite")).unwrap();
    assert!(!target.is_already_durable(&stored).unwrap());
    assert_eq!(stored.query_row("SELECT count(*) FROM messages", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
}

#[test]
fn another_tables_unique_constraint_is_not_session_isolation() {
    unrelated_constraint_is_fatal("INSERT INTO unrelated_unique VALUES(1);");
}

#[test]
fn trigger_text_cannot_impersonate_the_transcript_unique_constraint() {
    unrelated_constraint_is_fatal("SELECT RAISE(ABORT, 'UNIQUE constraint failed: messages.session_id, messages.message_id');");
}

#[test]
fn shutdown_reports_rejected_transcript_until_producer_correction() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for sparse in [false, true] {
        let mut fixture = FenceFixture::new();
        let (bad, good) = {
            let mut inner = fixture.inner.lock().unwrap();
            let cwd = fixture.root.path().to_string_lossy().into_owned();
            let bad = inner.create_session(Agent::Codex, None, cwd.clone(), None, None).session.id;
            let good = inner.create_session(Agent::Codex, None, cwd, None, None).session.id;
            for id in ["body-0", "body-1", "body-2"] {
                push_message_on_record(inner.session_mut(&bad).unwrap(), message(id));
            }
            (bad, good)
        };
        fixture.write(&fixture.collect()).unwrap();
        {
            let mut inner = fixture.inner.lock().unwrap();
            let record = inner.session_mut(&bad).unwrap();
            if sparse {
                record.session.messages.remove(0);
                record.session.messages.remove(0);
                record.message_start_index = 2;
                record.session.messages_loaded = false;
                record.session.messages[0] = message("body-0");
            } else {
                push_message_on_record(record, message("body-0"));
            }
            record.message_positions = build_message_positions(&record.session.messages);
            push_message_on_record(inner.session_mut(&good).unwrap(), message("healthy-shutdown"));
        }
        let mut state = fixture.app_state();
        state.inner = Arc::clone(&fixture.inner);
        let error = state.shutdown_persist_checked().unwrap_err();
        assert!(format!("{error:#}").contains(&bad));
        assert!(!state.persist_worker_alive.load(Ordering::Acquire));
        let connection = fixture.cache.connection.as_ref().unwrap();
        let ids = |session: &str| -> Vec<String> {
            connection.prepare("SELECT message_id FROM messages WHERE session_id=?1 ORDER BY position")
                .unwrap().query_map([session], |row| row.get(0)).unwrap()
                .collect::<rusqlite::Result<_>>().unwrap()
        };
        assert_eq!(ids(&bad), ["body-0", "body-1", "body-2"]);
        assert_eq!(ids(&good), ["healthy-shutdown"]);
        // A second caller must not infer success merely from the missing
        // worker handle. It must prove a corrected full cut again.
        assert!(state.shutdown_persist_checked().is_err());
        {
            let mut inner = fixture.inner.lock().unwrap();
            let record = inner.session_mut(&bad).unwrap();
            let index = if sparse { 0 } else { 3 };
            record.session.messages[index] = message(if sparse { "body-2" } else { "body-3" });
            record.message_positions = build_message_positions(&record.session.messages);
        }
        state.shutdown_persist_checked().unwrap();
        assert_eq!(ids(&bad), if sparse {
            vec!["body-0", "body-1", "body-2"]
        } else {
            vec!["body-0", "body-1", "body-2", "body-3"]
        });
    }
}

#[test]
fn shutdown_reports_real_store_failure() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let state = test_app_state();
    let blocked = state.persistence_path.with_file_name("directory-not-a-database");
    fs::create_dir(&blocked).unwrap();
    let state = AppState { persistence_path: Arc::new(blocked), ..state };
    assert!(state.shutdown_persist_checked().is_err());
    assert!(!state.persist_worker_alive.load(Ordering::Acquire));
}

fn partial_synchronous_fixture(remote_conflict: bool) -> (FenceFixture, AppState, String, String) {
    let mut fixture = FenceFixture::new();
    let (bad, good) = {
        let mut inner = fixture.inner.lock().unwrap();
        let cwd = fixture.root.path().to_string_lossy().into_owned();
        let bad = inner.create_session(Agent::Codex, None, cwd.clone(), None, None).session.id;
        let good = inner.create_session(Agent::Codex, None, cwd, None, None).session.id;
        let record = inner.session_mut(&bad).unwrap();
        if remote_conflict {
            record.remote_id = Some("remote-peer".into());
            record.remote_session_id = Some("upstream-bad".into());
        }
        for id in ["body-0", "body-1", "body-2"] {
            push_message_on_record(record, message(id));
        }
        let record = inner.session_mut(&good).unwrap();
        record.remote_id = Some("remote-peer".into());
        record.remote_session_id = Some("upstream-good".into());
        (bad, good)
    };
    fixture.write(&fixture.collect()).unwrap();
    {
        let mut inner = fixture.inner.lock().unwrap();
        let record = inner.session_mut(&bad).unwrap();
        record.session.messages.remove(0);
        record.session.messages.remove(0);
        record.message_start_index = 2;
        record.session.messages_loaded = false;
        record.session.messages[0] = other_message("body-0");
        record.message_positions = build_message_positions(&record.session.messages);
        push_message_on_record(inner.session_mut(&good).unwrap(), message("healthy-proxy-body"));
        inner.settings_persist_dirty = true;
        inner.remote_settings_persist_dirty = true;
        inner.remote_delta_persist_dirty = true;
        inner.preferences.default_codex_model = "saved-sibling-preference".into();
    }
    let mut state = fixture.app_state();
    state.inner = Arc::clone(&fixture.inner);
    state.persist_worker_alive.store(false, Ordering::Release);
    (fixture, state, bad, good)
}

#[test]
fn partial_synchronous_save_keeps_proxy_retry_without_poisoning_metadata() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (fixture, state, bad, good) = partial_synchronous_fixture(true);
    {
        let mut inner = state.inner.lock().unwrap();
        let (_, dispatch) = state.commit_persisted_delta_locked_with_dispatch(&mut inner).unwrap();
        assert_eq!(dispatch, PersistDispatch::Synchronous, "metadata committed despite a rejected row");
        assert!(!inner.settings_persist_dirty);
        assert!(!inner.remote_settings_persist_dirty);
        assert!(inner.remote_delta_persist_dirty, "a rejected proxy row is not durable");
        let error = state.retry_remote_delta_persist_if_dirty_locked(&mut inner).unwrap_err();
        assert!(format!("{error:#}").contains(&bad));
        assert!(format!("{error:#}").contains("UNIQUE constraint failed"));
        assert!(inner.remote_delta_persist_dirty);
        assert!(state.commit_remote_delta_persisted_locked(&mut inner).is_err());
        assert!(inner.remote_delta_persist_dirty);
        inner.remote_delta_persist_dirty = false;
        state.commit_delta_locked(&mut inner).unwrap();
        assert!(inner.remote_delta_persist_dirty, "post-worker streaming must retain rejected proxy duty");
    }
    let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    assert_eq!(loaded.preferences.default_codex_model, "saved-sibling-preference");
    let good_record = &loaded.sessions[loaded.find_session_index(&good).unwrap()];
    assert_eq!(good_record.session.messages[0].id(), "healthy-proxy-body");
    let bad_record = &loaded.sessions[loaded.find_session_index(&bad).unwrap()];
    assert_eq!(bad_record.session.messages.len(), 3);
    let diagnostic = state.persist_observer.snapshot_at(std::time::Instant::now());
    assert_eq!(diagnostic.rejected_session_writes, 4);
    assert!(diagnostic.last_session_rejection.unwrap().error.contains("messages.message_id"));
    {
        let mut inner = state.inner.lock().unwrap();
        let record = inner.session_mut(&bad).unwrap();
        record.session.messages[0] = message("body-2");
        record.message_positions = build_message_positions(&record.session.messages);
        state.retry_remote_delta_persist_if_dirty_locked(&mut inner).unwrap();
        assert!(!inner.remote_delta_persist_dirty);
    }
    drop(fixture);
}

#[test]
fn local_rejection_does_not_poison_a_saved_proxy_retry() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (_fixture, state, _, _) = partial_synchronous_fixture(false);
    let mut inner = state.inner.lock().unwrap();
    state.retry_remote_delta_persist_if_dirty_locked(&mut inner).unwrap();
    assert!(!inner.remote_delta_persist_dirty);
    assert_eq!(state.persist_observer.snapshot_at(std::time::Instant::now()).rejected_session_writes, 1);
}

#[test]
fn rejection_diagnostic_is_bounded_and_survives_a_healthy_tick() {
    let observer = Arc::new(PersistObserver::default());
    let rejected = PersistSessionRejection { session_id: "s".repeat(300), error: "é".repeat(3000) };
    observer.record_session_rejections(&[rejected]);
    let observation = observer.begin_at(std::time::Instant::now());
    observation.finish(true, &PersistTickTimings::default());
    let snapshot = observer.snapshot_at(std::time::Instant::now());
    assert_eq!(snapshot.completed_ticks, 1);
    assert_eq!(snapshot.failed_ticks, 0);
    assert_eq!(snapshot.rejected_session_writes, 1);
    let json = serde_json::to_value(snapshot).unwrap();
    assert_eq!(json["lastSessionRejection"]["sessionId"].as_str().unwrap().len(), 128);
    assert_eq!(json["lastSessionRejection"]["error"].as_str().unwrap().chars().count(), 1024);
}

fn queue_admission_owner(record: &mut SessionRecord) -> EngramQueuedAdmissionOwner {
    let pending = PendingPrompt {
        id: format!("ack-head-{}", record.session.id),
        timestamp: stamp_now(),
        text: "keep the original queued intent".into(),
        expanded_text: None,
        source: None,
        attachments: Vec::new(),
        engram_interrupted: false,
        is_engram_retained: false,
    };
    queue_prompt_on_record(record, pending, Vec::new());
    EngramQueuedAdmissionOwner::capture(record).unwrap()
}

#[test]
fn synchronous_admission_ack_is_scoped_to_the_saved_session() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (_fixture, state, bad, good) = partial_synchronous_fixture(false);
    let (bad_owner, good_owner) = {
        let mut inner = state.inner.lock().unwrap();
        let bad_owner = queue_admission_owner(inner.session_mut(&bad).unwrap());
        let good_owner = queue_admission_owner(inner.session_mut(&good).unwrap());
        (bad_owner, good_owner)
    };
    let deadline = || state.engram_budget_clock().now() + phase_sync::DEADLOCK_GUARD;
    let (error, fault) = state.confirm_engram_admission_durable_classified(
        &bad, deadline(), &bad_owner,
    ).unwrap_err();
    assert!(error.message.contains(&bad));
    assert!(error.message.contains("UNIQUE constraint failed"));
    assert!(matches!(fault, EngramAdmissionDurabilityFault::Local));
    state.confirm_engram_admission_durable_classified(&good, deadline(), &good_owner).unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        let record = inner.session_mut(&bad).unwrap();
        record.session.messages[0] = message("body-2");
        record.message_positions = build_message_positions(&record.session.messages);
    }
    state.confirm_engram_admission_durable_classified(&bad, deadline(), &bad_owner).unwrap();
    let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    for id in [&bad, &good] {
        assert_eq!(loaded.sessions[loaded.find_session_index(id).unwrap()]
            .queued_prompts.front().unwrap().pending_prompt.text, "keep the original queued intent");
    }
}

#[test]
fn synchronous_retry_ack_does_not_mark_a_rejected_session_saved() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (_fixture, state, bad, good) = partial_synchronous_fixture(false);
    let mut inner = state.inner.lock().unwrap();
    for id in [&bad, &good] {
        let record = inner.session_mut(id).unwrap();
        let owner = queue_admission_owner(record);
        record.engram.abort_retry = Some(EngramAbortRetry {
            prompt_id: owner.prompt_id,
            reason: EngramAbortReason::BeginPreparation,
            settled_grant_id: None,
            attempts: 1,
            held_since: stamp_now(),
            due_at: stamp_now(),
            authority: "same-retry-authority".into(),
            acknowledged: false,
            generation: record.engram.dispatch_generation,
        });
    }
    for id in [&bad, &good] {
        let index = inner.find_session_index(id).unwrap();
        state.request_engram_abort_acknowledgement_locked(&mut inner, index);
    }
    assert!(!inner.sessions[inner.find_session_index(&bad).unwrap()].engram.abort_retry_saved);
    assert!(inner.sessions[inner.find_session_index(&good).unwrap()].engram.abort_retry_saved);
    let record = inner.session_mut(&bad).unwrap();
    record.session.messages[0] = message("body-2");
    record.message_positions = build_message_positions(&record.session.messages);
    let index = inner.find_session_index(&bad).unwrap();
    state.request_engram_abort_acknowledgement_locked(&mut inner, index);
    assert!(inner.sessions[index].engram.abort_retry_saved);
}

#[test]
fn synchronous_capacity_ack_does_not_mark_a_rejected_session_saved() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (_fixture, state, bad, good) = partial_synchronous_fixture(false);
    let mut inner = state.inner.lock().unwrap();
    for id in [&bad, &good] {
        let record = inner.session_mut(id).unwrap();
        record.provider_capacity_retry = Some(ProviderCapacityRetry {
            nonce: format!("capacity-{id}"),
            server_instance: state.server_instance_id.clone(),
            runtime_id: "retained-capacity-runtime".into(),
            generation: record.active_turn_generation,
            stop_generation: record.runtime_stop_generation,
            pause_epoch: record.auto_dispatch_pause_epoch,
            dispatch_generation: record.engram.dispatch_generation,
            settings: provider_capacity_settings(record),
            attempts: 0,
            due_at: None,
            recovering: false,
            dispatched: false,
            original_preview: "provider at capacity".into(),
            saved: false,
            fence: None,
        });
    }
    for id in [&bad, &good] {
        let index = inner.find_session_index(id).unwrap();
        state.request_provider_capacity_ack_locked(&mut inner, index);
    }
    assert!(!inner.sessions[inner.find_session_index(&bad).unwrap()]
        .provider_capacity_retry.as_ref().unwrap().saved);
    assert!(inner.sessions[inner.find_session_index(&good).unwrap()]
        .provider_capacity_retry.as_ref().unwrap().saved);
    let record = inner.session_mut(&bad).unwrap();
    record.session.messages[0] = message("body-2");
    record.message_positions = build_message_positions(&record.session.messages);
    let index = inner.find_session_index(&bad).unwrap();
    state.request_provider_capacity_ack_locked(&mut inner, index);
    assert!(inner.sessions[index].provider_capacity_retry.as_ref().unwrap().saved);
}

#[test]
fn row_bound_commits_refuse_only_their_rejected_session() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (_fixture, state, bad, good) = partial_synchronous_fixture(false);
    let mut inner = state.inner.lock().unwrap();
    inner.session_mut(&bad).unwrap().engram.routing_token = Some("unsaved-bad-token".into());
    inner.session_mut(&good).unwrap().engram.routing_token = Some("saved-good-token".into());
    let error = state.commit_session_locked(&mut inner, &bad).unwrap_err();
    assert!(format!("{error:#}").contains(&bad));
    assert!(format!("{error:#}").contains("UNIQUE constraint failed"));
    state.commit_session_locked(&mut inner, &good).unwrap();
    assert!(!inner.settings_persist_dirty);
    assert!(!inner.remote_settings_persist_dirty);
    drop(inner);
    let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    assert!(loaded.sessions[loaded.find_session_index(&bad).unwrap()].engram.routing_token.is_none());
    assert_eq!(loaded.sessions[loaded.find_session_index(&good).unwrap()].engram.routing_token.as_deref(),
        Some("saved-good-token"));
    assert_eq!(loaded.preferences.default_codex_model, "saved-sibling-preference");
}
