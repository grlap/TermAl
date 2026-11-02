//! Durable session writes must reach the production delta persist.
//!
//! Owns: `DurableSessionSnapshot`, a guard any test can take before an action
//! and check after it, plus the regression tests that use it for writes once
//! made through a bare `sessions[i]` borrow without a mutation stamp.
//! Does not own: the delta plan and its materialization (tests/persist_delta.rs)
//! or the behaviour of the paths exercised here.
//! New module, not a split.
//!
//! Tests normally run with a disconnected persist channel, so every commit
//! falls back to a full synchronous save, which hides a missing stamp. In
//! production a commit only wakes the delta worker, and that worker selects
//! only sessions whose `mutation_stamp` passed its watermark
//! (`StateInner::collect_persist_delta_plan`).

use super::*;

/// Each session's durable form and the stamp watermark at one moment.
pub(super) struct DurableSessionSnapshot {
    watermark: u64,
    durable: BTreeMap<String, String>,
    baseline: PersistedState,
}

/// A session exactly as storage would hold it.
fn durable_session_json(record: &SessionRecord) -> String {
    serde_json::to_string(&PersistedSessionRecord::from_record(record))
        .expect("a session record serializes")
}

impl DurableSessionSnapshot {
    pub(super) fn take(state: &AppState) -> Self {
        let inner = state.inner.lock().expect("state mutex poisoned");
        Self {
            watermark: inner.last_mutation_stamp,
            baseline: PersistedState::from_inner(&inner),
            durable: inner
                .sessions
                .iter()
                .map(|record| (record.session.id.clone(), durable_session_json(record)))
                .collect(),
        }
    }

    /// Every visible session whose durable form changed since the snapshot
    /// must be one the delta persist selects at the snapshot's watermark.
    pub(super) fn assert_changes_reach_the_delta(&self, state: &AppState) {
        // Collect under the lock and assert after releasing it: a failed
        // assertion must not poison the state mutex for the caller.
        let (missed, changed_ids): (Vec<String>, Vec<String>) = {
            let inner = state.inner.lock().expect("state mutex poisoned");
            let changed: Vec<_> = inner
                .sessions
                .iter()
                .filter(|record| !record.hidden)
                .filter(|record| {
                    self.durable.get(&record.session.id) != Some(&durable_session_json(record))
                })
                .collect();
            let missed = changed.iter()
                .filter(|record| record.mutation_stamp <= self.watermark)
                .map(|record| record.session.id.clone())
                .collect();
            let changed_ids = changed.iter().map(|record| record.session.id.clone()).collect();
            (missed, changed_ids)
        };
        assert!(
            missed.is_empty(),
            "durable session changes the delta persist would skip: {missed:?}"
        );

        // Exercise the selected session rows against a disposable SQLite
        // baseline, independently of a fixture's synchronous-save fallback.
        // Metadata belongs to this scratch store; session selection and rows
        // below are the unmodified production delta collected at the watermark.
        let root = TestTempRoot::create("session-stamp-delta-reload");
        let path = root.path().join("termal.sqlite");
        let mut baseline = self.baseline.clone();
        let baseline_image = serde_json::to_value(&baseline).unwrap();
        let connection = open_sqlite_state_connection_unconfigured(&path).unwrap();
        assert!(ensure_sqlite_state_schema_for_path_with_initialization(&connection, &path).unwrap(),
            "this helper initializes a new disposable destination, never an existing store");
        let initialized_generation = metadata_unit_generation(&connection).unwrap();
        assert_eq!(initialized_generation, 1);
        if let Some(identity) = &baseline.federation.local {
            // A federation-enabled test image must retain its own identity.
            // Bootstrap only this brand-new scratch store to that fixture's
            // identity before its first image; never rewrite the source image
            // or bypass the production writer's host/predecessor checks.
            assert_eq!(federation_meta_version(&connection).unwrap(), 0);
            let transaction = rusqlite::Transaction::new_unchecked(
                &connection, rusqlite::TransactionBehavior::Immediate,
            ).unwrap();
            transaction.execute("UPDATE meta SET value=?1 WHERE key='federation_host_id'",
                [&identity.host_id]).unwrap();
            transaction.execute("UPDATE meta SET value=?1 WHERE key='federation_host_generation'",
                [identity.host_generation.to_string()]).unwrap();
            transaction.commit().unwrap();
            assert_eq!(read_federation_host_identity(&connection).unwrap(), *identity);
        }
        baseline.metadata_capture = PersistMetadataCapture::new(Arc::new(
            AtomicU64::new(initialized_generation),
        ));
        assert_eq!(serde_json::to_value(&baseline).unwrap(), baseline_image,
            "destination setup preserves the complete serialized baseline");
        drop(connection);
        let generation = persist_state_from_persisted(&path, &baseline).unwrap();
        let mut delta = collect_persist_delta_from_shared_state(&state.inner, self.watermark);
        for id in changed_ids {
            assert!(delta.changed_sessions.iter().any(|record| record.session.id == id),
                "the production delta selects the changed durable session {id}");
        }
        delta.metadata = baseline.metadata_only();
        delta.metadata.metadata_capture = PersistMetadataCapture::new(Arc::new(
            AtomicU64::new(generation),
        ));
        persist_delta_via_cache(&mut SqlitePersistConnectionCache::new(), &path, &delta).unwrap();
        let connection = open_sqlite_state_connection_unconfigured(&path).unwrap();
        let reloaded = load_session_records_from_sqlite(&connection, &path).unwrap();
        for selected in &delta.changed_sessions {
            let before = self.durable.get(&selected.session.id)
                .map(|json| serde_json::from_str::<Value>(json).unwrap());
            let expected = serde_json::to_value(selected).unwrap();
            let actual = serde_json::to_value(reloaded.iter()
                .find(|record| record.session.id == selected.session.id)
                .expect("the selected session reloads from SQLite")).unwrap();
            let keys: BTreeSet<_> = expected.as_object().unwrap().keys()
                .chain(before.as_ref().and_then(Value::as_object).into_iter().flat_map(|object| object.keys()))
                .collect();
            for key in keys {
                if before.as_ref().and_then(|before| before.get(key)) != expected.get(key) {
                    assert_eq!(actual.get(key), expected.get(key),
                        "changed durable field {key} reloads for {}", selected.session.id);
                }
            }
        }
    }
}

fn rfc3339_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[test]
fn the_guard_flags_only_an_unstamped_durable_write() {
    let state = test_app_state();
    let session = test_session_id(&state, Agent::Codex);
    let snapshot = DurableSessionSnapshot::take(&state);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram.operator_paused = true;
    }
    let flagged = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        snapshot.assert_changes_reach_the_delta(&state)
    }));
    assert!(flagged.is_err(), "an unstamped durable write is flagged");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.stamp_session_at_index(index);
    }
    snapshot.assert_changes_reach_the_delta(&state);
}

#[test]
fn stopping_a_stale_abort_retry_persists_its_removal() {
    let state = test_app_state();
    let session = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        // A retry left behind by a head since cancelled.
        inner.session_mut_by_index(index).unwrap().engram.abort_retry = Some(EngramAbortRetry {
            prompt_id: "a-head-since-cancelled".to_owned(),
            reason: EngramAbortReason::AdmissionFence,
            settled_grant_id: None,
            attempts: 1,
            held_since: rfc3339_now(),
            due_at: rfc3339_now(),
            authority: "the-aborted-admission".to_owned(),
            acknowledged: true,
            generation: 0,
        });
    }
    let snapshot = DurableSessionSnapshot::take(&state);

    assert!(!state.stop_engram_abort_retry(&session).unwrap());

    {
        let inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        assert!(inner.sessions[index].engram.abort_retry.is_none());
    }
    snapshot.assert_changes_reach_the_delta(&state);
}

#[test]
fn a_failed_stop_commit_restamps_the_restored_capacity_clear() {
    let mut state = test_app_state();
    let session = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.session.status = SessionStatus::Active;
        record.provider_capacity_retry = Some(ProviderCapacityRetry {
            nonce: "capacity-nonce".to_owned(),
            server_instance: "this-host".to_owned(),
            runtime_id: "capacity-runtime".to_owned(),
            generation: 1,
            stop_generation: 0,
            pause_epoch: 0,
            dispatch_generation: 0,
            settings: "settings".to_owned(),
            attempts: 1,
            due_at: Some(rfc3339_now()),
            recovering: false,
            dispatched: false,
            original_preview: "working".to_owned(),
            saved: false,
            fence: None,
        });
    }
    // A directory where the store should be fails the synchronous save, so
    // the Stop's commit fails and its rollback runs.
    let failing = TestTempRoot::create("stop-commit-fails");
    let directory = failing.path().join("persistence-is-a-directory");
    fs::create_dir_all(&directory).unwrap();
    state.persistence_path = Arc::new(directory);
    let snapshot = DurableSessionSnapshot::take(&state);

    assert!(state
        .begin_requested_stop_session(&session, &StopSessionOptions::default())
        .is_err());

    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.provider_capacity_retry.is_none(), "the clear made before the copy stays");
        assert_eq!(record.session.status, SessionStatus::Active, "the Stop rolled back");
    }
    snapshot.assert_changes_reach_the_delta(&state);
}
