// Child of deferred_state_snapshot.rs: observes per-session projection and
// cache validation at the real summary read, and protects retained cut inputs.
// Does not own the shared-row container, runtime cleanup or broadcast ordering.
use super::*;

struct SessionWorkProbe {
    validation: bool,
}

impl SessionWorkProbe {
    fn install(
        state: &AppState,
        validation: bool,
        held: Arc<AtomicUsize>,
        released: Arc<AtomicUsize>,
    ) -> Self {
        let state = state.clone();
        let hook = if validation {
            &TEST_BEFORE_SESSION_SUMMARY_VALIDATE
        } else {
            &TEST_BEFORE_SESSION_SUMMARY_PROJECT
        };
        hook.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some(Box::new(move || {
                if state.inner.is_not_held_by_current_thread_for_test() {
                    released.fetch_add(1, Ordering::SeqCst);
                } else {
                    held.fetch_add(1, Ordering::SeqCst);
                }
            }));
        });
        Self { validation }
    }
}

impl Drop for SessionWorkProbe {
    fn drop(&mut self) {
        let hook = if self.validation {
            &TEST_BEFORE_SESSION_SUMMARY_VALIDATE
        } else {
            &TEST_BEFORE_SESSION_SUMMARY_PROJECT
        };
        hook.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

fn summary_fixture(count: usize) -> AppState {
    let state = test_app_state();
    for ordinal in 0..count {
        let id = test_session_id(&state, Agent::Codex);
        let mut inner = state.inner.lock().unwrap();
        let record = inner.session_mut(&id).unwrap();
        record.session.name = format!("cut session {ordinal}");
        record.session.preview = "retained preview".to_owned();
        record.queued_prompts = vec![queue_projection_cache::queued("first")].into();
        record.engram_mcp_revocation_pending = ordinal % 2 == 0;
    }
    let _ = state.agent_readiness_snapshot();
    state
}

fn assert_session_work_is_off_lock(warm: bool) {
    // Two populations make an accidental zero-observation pass visible and
    // show that the counted work is per-session, without a wall-clock bound.
    for count in [8, 1] {
        let state = summary_fixture(count);
        if warm {
            state.warm_state_projection_cache();
        } else {
            session_summary_cache(&state)
                .lock()
                .unwrap()
                .sessions
                .clear();
        }
        let expected = state.snapshot_from_inner(&state.inner.lock().unwrap());
        let held = Arc::new(AtomicUsize::new(0));
        let released = Arc::new(AtomicUsize::new(0));
        let probe = SessionWorkProbe::install(&state, warm, held.clone(), released.clone());
        let actual = state.summary_snapshot();
        drop(probe);
        assert_eq!(
            serde_json::to_value(&actual).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        let locked = held.load(Ordering::SeqCst);
        let unlocked = released.load(Ordering::SeqCst);
        eprintln!(
            "summary_cut_work warm={warm} sessions={count} held={locked} released={unlocked}"
        );
        assert!(
            locked + unlocked >= count,
            "the observer must see every session's work"
        );
        assert_eq!(
            locked, 0,
            "per-session summary work ran under StateInner (warm={warm})"
        );
    }
}

#[test]
fn summary_cut_cold_projection_runs_after_state_inner_release() {
    assert_session_work_is_off_lock(false);
}

#[test]
fn summary_cut_warm_validation_runs_after_state_inner_release() {
    assert_session_work_is_off_lock(true);
}

#[test]
fn summary_cut_concurrent_unstamped_mutation_preserves_metadata_queue_and_revocation_inputs() {
    let state = summary_fixture(3);
    let expected = state.snapshot_from_inner(&state.inner.lock().unwrap());
    let session = expected.sessions[0].id.clone();
    let successor_state = state.clone();
    let successor_session = session.clone();
    state
        .inner
        .lock()
        .unwrap()
        .state_projection_cache
        .inputs
        .lock()
        .unwrap()
        .before_materialize = Some(Box::new(move || {
        assert!(
            successor_state
                .inner
                .is_not_held_by_current_thread_for_test()
        );
        std::thread::spawn(move || {
            let mut inner = successor_state.inner.lock().unwrap();
            let index = inner.find_session_index(&successor_session).unwrap();
            // Deliberately bypass the stamping accessor: cache correctness
            // must still follow the captured fields, not a stamp alone.
            let record = &mut inner.sessions[index];
            let stamp = record.mutation_stamp;
            record.session.name = "concurrent successor".to_owned();
            record.queued_prompts[0].pending_prompt.id = "replacement".to_owned();
            record.orchestrator_auto_dispatch_blocked = true;
            record.engram_mcp_revocation_pending = !record.engram_mcp_revocation_pending;
            assert_eq!(record.mutation_stamp, stamp);
            inner.revision += 1;
        })
        .join()
        .unwrap();
    }));
    let retained = state.summary_snapshot();
    assert_eq!(
        serde_json::to_value(&retained).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    let successor = state.summary_snapshot();
    let changed = successor
        .sessions
        .iter()
        .find(|row| row.id == session)
        .unwrap();
    assert_eq!(successor.revision, retained.revision + 1);
    assert_eq!(changed.name, "concurrent successor");
    assert!(changed.queue_paused);
    assert_ne!(
        changed.queue_projection_hash,
        retained.sessions[0].queue_projection_hash
    );
    assert_ne!(
        successor.pending_engram_mcp_revocation_session_ids,
        retained.pending_engram_mcp_revocation_session_ids
    );
}

#[test]
fn summary_cut_reverse_materialization_keeps_each_revision_and_successor_cache() {
    let state = summary_fixture(2);
    let expected_old = state.snapshot_from_inner(&state.inner.lock().unwrap());
    let old = capture_cut(&state);
    {
        let mut inner = state.inner.lock().unwrap();
        inner.sessions[0].session.name = "new cut".to_owned();
        inner.sessions[0].engram_mcp_revocation_pending = false;
        inner.revision += 1;
    }
    let expected_new = state.snapshot_from_inner(&state.inner.lock().unwrap());
    let mut new = capture_cut(&state);
    new.project_sessions();
    let published_row = new.sessions[0].clone();
    assert_eq!(
        serde_json::to_value(new.materialize()).unwrap(),
        serde_json::to_value(&expected_new).unwrap()
    );
    assert_eq!(
        serde_json::to_value(old.materialize()).unwrap(),
        serde_json::to_value(expected_old).unwrap()
    );
    let successor = current_cut(&state);
    assert!(Arc::ptr_eq(&published_row, &successor.sessions[0]));
    assert_eq!(
        serde_json::to_value(successor.materialize()).unwrap(),
        serde_json::to_value(expected_new).unwrap()
    );
}

#[test]
fn summary_cut_writer_can_commit_while_the_session_cache_is_busy() {
    let (state, session, mailbox) = cached_state();
    mailbox.take_pending_for_test();
    let cache = session_summary_cache(&state);
    let busy = cache.lock().unwrap();
    let writer = state.clone();
    let (committed, complete) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut inner = writer.inner.lock().unwrap();
        inner.session_mut(&session).unwrap().session.name = "writer progressed".to_owned();
        writer.commit_locked(&mut inner).unwrap();
        committed.send(()).unwrap();
    });

    // Keep the cache locked until the writer reports a completed commit.
    // The deadline only bounds a broken test. Release before joining even
    // when the writer cannot complete while the cache is held.
    let committed_while_busy = complete.recv_timeout(Duration::from_secs(5));
    drop(busy);
    worker.join().unwrap();
    committed_while_busy.expect("commit waited for the session summary cache");
    let snapshot = materialize(mailbox.take_pending_for_test().pop().unwrap());
    assert_eq!(snapshot.sessions[0].name, "writer progressed");
}
