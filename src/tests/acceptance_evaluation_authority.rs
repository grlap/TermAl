use super::*;

fn final_publication_expiry_keeps_candidate(recovery: bool) {
    let (state, project, _, root) = fixture();
    install_store(&state, &project, &root);
    let store = established_store(&state, &project).unwrap();
    let owner = state.inner.lock().unwrap().engram_work_naming_history[0]
        .transition
        .as_ref()
        .unwrap()
        .clone();
    let clock = state.engram_budget_clock();

    if recovery {
        // Commit a real Candidate, but lose permission to publish it before
        // ACK validation completes. The next prepare must recover that image.
        let expiry_clock = clock.clone();
        state
            .inner
            .lock()
            .unwrap()
            .test_engram_authority_ack_boundary = Some(Arc::new(move |phase| {
            if phase == "before_owner_check" {
                expiry_clock.advance(Duration::from_secs(2));
            }
        }));
        let error = state
            .publish_engram_authority(&store, &owner, Duration::from_secs(2))
            .unwrap_err();
        assert!(error.message.contains("acknowledgement exceeded"));
    }

    let boundary = if recovery {
        "before_recovery_publication"
    } else {
        "before_publication"
    };
    let expiry_clock = clock.clone();
    let reached = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hook_reached = reached.clone();
    state
        .inner
        .lock()
        .unwrap()
        .test_engram_authority_ack_boundary = Some(Arc::new(move |phase| {
        if phase == boundary {
            assert!(!hook_reached.swap(true, std::sync::atomic::Ordering::SeqCst));
            // ACK and its owner check have succeeded. Only the final
            // publication lock remains, under the original deadline.
            expiry_clock.advance(Duration::from_secs(2));
        }
    }));
    let writer = sqlite_state_write_lock(state.persistence_path.as_path());
    let tickets_before = sqlite_state_writer_issued_tickets(&writer);
    let deadline = clock.now() + Duration::from_secs(2);
    let result = if recovery {
        state
            .prepare_engram_authority_until(&store, &owner.binding, deadline)
            .map(|_| ())
    } else {
        state.publish_engram_authority_until(&store, &owner, deadline)
    };
    assert!(reached.load(std::sync::atomic::Ordering::SeqCst));
    assert!(sqlite_state_writer_issued_tickets(&writer) > tickets_before);
    {
        let inner = state.inner.lock().unwrap();
        let retained = inner.engram_work_naming_history[0]
            .transition
            .as_ref()
            .unwrap();
        assert_eq!(retained.id, owner.id);
        assert_eq!(retained.version, owner.version);
        assert_eq!(retained.phase, EngramAuthorityPhase::Candidate);
        assert!(engram_authority_work_unresolved(
            &inner,
            &store,
            &owner.binding.work_id
        ));
    }
    let error = result.unwrap_err();
    assert!(error.message.contains("publication exceeded"), "{error:?}");

    // A later operation can recover the exact durable Candidate; expiry
    // must neither discard it nor poison a fresh operation's budget.
    state
        .inner
        .lock()
        .unwrap()
        .test_engram_authority_ack_boundary = None;
    if recovery {
        let fresh = state
            .prepare_engram_authority(&store, &owner.binding, Duration::from_secs(2))
            .unwrap();
        assert_ne!(fresh.id, owner.id);
        assert_eq!(fresh.phase, EngramAuthorityPhase::Prepared);
    } else {
        state
            .publish_engram_authority(&store, &owner, Duration::from_secs(2))
            .unwrap();
        assert_eq!(
            state.inner.lock().unwrap().engram_work_naming_history[0]
                .transition
                .as_ref()
                .unwrap()
                .phase,
            EngramAuthorityPhase::Published
        );
    }
}

#[test]
fn fixture_authority_final_publication_rechecks_the_enclosing_deadline() {
    final_publication_expiry_keeps_candidate(false);
}

#[test]
fn fixture_authority_final_recovery_publication_rechecks_the_enclosing_deadline() {
    final_publication_expiry_keeps_candidate(true);
}

#[test]
fn fixture_authority_scripted_budget_survives_delta_and_owner_lock_waits() {
    for boundary in ["before_delta", "before_owner_check"] {
        let (state, project, _, root) = fixture();
        install_store(&state, &project, &root);
        let store = established_store(&state, &project).unwrap();
        let clock = state.engram_budget_clock();
        let started = clock.now();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let hook_gate = gate.clone();
        let once = std::sync::atomic::AtomicBool::new(false);
        state
            .inner
            .lock()
            .unwrap()
            .test_engram_authority_ack_boundary = Some(Arc::new(move |phase| {
            if phase == boundary && !once.swap(true, std::sync::atomic::Ordering::SeqCst) {
                entered_tx.send(()).unwrap();
                let (ready, changed) = &*hook_gate;
                let mut ready = ready.lock().unwrap();
                while !*ready {
                    ready = changed.wait(ready).unwrap();
                }
            }
        }));
        let worker = state.clone();
        let worker_store = store.clone();
        let task = std::thread::spawn(move || {
            acknowledge_fixture_work(
                &worker,
                &worker_store,
                &evidence_selection::canonical_core_receipt(),
            );
        });
        entered_rx.recv_timeout(Duration::from_secs(30)).unwrap();
        let held = state.inner.lock().unwrap();
        {
            let (ready, changed) = &*gate;
            *ready.lock().unwrap() = true;
            changed.notify_all();
        }
        // The worker's next operation needs this actual StateInner lock.
        // Hold it beyond the old total allowance, including time outside SQLite.
        std::thread::sleep(Duration::from_secs(2));
        assert_eq!(clock.now(), started);
        drop(held);
        task.join()
            .expect("positive scripted setup survives real lock scheduling");
        let mut inner = state.inner.lock().unwrap();
        inner.test_engram_authority_ack_boundary = None;
        assert_eq!(
            inner.engram_work_naming_history[0]
                .transition
                .as_ref()
                .unwrap()
                .phase,
            EngramAuthorityPhase::Published
        );
    }
}

#[test]
fn fixture_authority_scripted_expiry_rejects_a_real_late_commit() {
    let (state, project, _, root) = fixture();
    install_store(&state, &project, &root);
    let store = established_store(&state, &project).unwrap();
    let binding = state.inner.lock().unwrap().engram_work_naming_history[0]
        .transition
        .as_ref()
        .unwrap()
        .binding
        .clone();
    let clock = state.engram_budget_clock();
    let writer = sqlite_state_write_lock(state.persistence_path.as_path());
    let held = lock_sqlite_state_writer(&writer);
    let ticket = sqlite_state_writer_issued_tickets(&writer) + 1;
    let worker = state.clone();
    let worker_store = store.clone();
    let task = std::thread::spawn(move || {
        worker.prepare_engram_authority(&worker_store, &binding, Duration::from_secs(2))
    });
    wait_for_sqlite_state_writer_issued_tickets(&writer, ticket);
    clock.advance(Duration::from_secs(2));
    drop(held);
    let error = task.join().unwrap().unwrap_err();
    assert!(
        error.message.contains("acknowledgement exceeded"),
        "{error:?}"
    );
    let inner = state.inner.lock().unwrap();
    assert_eq!(
        inner.engram_work_naming_history[0]
            .transition
            .as_ref()
            .unwrap()
            .phase,
        EngramAuthorityPhase::Prepared
    );
    assert!(engram_authority_work_unresolved(
        &inner,
        &store,
        &inner.engram_work_naming_history[0].work_id
    ));
}

#[test]
fn fixture_budget_drivers_wake_on_clock_advance_and_real_completion() {
    let clock = EngramBudgetClock::scripted();
    let (fence, waiter) = PersistFence::new_with_clock(
        PersistFenceTarget::TestRunCardsEpoch("fixture-driver".to_owned()),
        clock.now() + Duration::from_secs(2),
        clock.clone(),
    );
    let task = std::thread::spawn(move || waiter.wait());
    clock.wait_for_scripted_waiter();
    clock.advance(Duration::from_secs(2));
    assert_eq!(task.join().unwrap(), Err(PersistFenceError::Deadline));
    fence.finish(Ok(()));
    assert_eq!(
        fence.completion.poll_at(clock.now()),
        Some(Err(PersistFenceError::Deadline))
    );

    // The boot coordinator's driver uses this same completion channel path.
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker_clock = clock.clone();
    let deadline = clock.now() + Duration::from_secs(2);
    let task = std::thread::spawn(move || worker_clock.recv_until(&receiver, deadline));
    clock.wait_for_scripted_waiter();
    sender.send("completed").unwrap();
    clock.notify();
    assert_eq!(task.join().unwrap().unwrap(), "completed");

    let (_sender, receiver) = std::sync::mpsc::channel::<()>();
    let worker_clock = clock.clone();
    let deadline = clock.now() + Duration::from_secs(2);
    let task = std::thread::spawn(move || worker_clock.recv_until(&receiver, deadline));
    clock.wait_for_scripted_waiter();
    clock.advance(Duration::from_secs(2));
    assert_eq!(
        task.join().unwrap(),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    );
}

#[test]
fn fixture_authority_acknowledgement_excludes_stalled_manual_persistence() {
    let (state, project, _, root) = fixture();
    state.install_test_engram_budget_clock(EngramBudgetClock::scripted());
    super::super::work_visualizer::install_store(&state, &project, &root);
    let store = established_store(&state, &project).unwrap();
    let writer = sqlite_state_write_lock(state.persistence_path.as_path());
    let held = lock_sqlite_state_writer(&writer);
    let ticket = sqlite_state_writer_issued_tickets(&writer) + 1;
    let worker_state = state.clone();
    let worker_store = store.clone();
    let task = std::thread::spawn(move || {
        acknowledge_fixture_work(
            &worker_state,
            &worker_store,
            &evidence_selection::canonical_core_receipt(),
        );
    });
    wait_for_sqlite_state_writer_issued_tickets(&writer, ticket);
    // The actual writer has entered synchronous persistence. Keep its ticket
    // blocked for the entire original fixture allowance, guaranteeing that
    // acknowledgement happens after that deadline rather than racing it.
    std::thread::sleep(Duration::from_secs(2));
    drop(held);
    task.join()
        .expect("fixture persistence must not consume its authority budget");
    let inner = state.inner.lock().unwrap();
    let history = inner
        .engram_work_naming_history
        .iter()
        .find(|history| {
            history.store == store
                && history.work_id
                    == evidence_selection::canonical_core_receipt()["status"]["work"]["work_id"]
                        .as_str()
                        .unwrap()
        })
        .unwrap();
    assert_eq!(history.epoch, ENGRAM_NAMING_HISTORY_EPOCH);
    assert_eq!(
        history.transition.as_ref().unwrap().phase,
        EngramAuthorityPhase::Published
    );
    assert!(!engram_authority_work_unresolved(
        &inner,
        &store,
        &history.work_id
    ));
    assert_eq!(history.proofs.len(), 1);
}

#[test]
fn fixture_authority_budget_keeps_expired_and_connected_acknowledgements_withheld() {
    let (mut state, project, _, root) = fixture();
    install_store(&state, &project, &root);
    let store = established_store(&state, &project).unwrap();
    let binding = state.inner.lock().unwrap().engram_work_naming_history[0]
        .transition
        .as_ref()
        .unwrap()
        .binding
        .clone();
    let error = state
        .prepare_engram_authority_until(
            &store,
            &binding,
            state.engram_budget_clock().now() - Duration::from_secs(1),
        )
        .unwrap_err();
    assert!(
        error.message.contains("acknowledgement exceeded"),
        "{error:?}"
    );

    state.install_test_engram_budget_clock(EngramBudgetClock::Real);
    let writer = sqlite_state_write_lock(state.persistence_path.as_path());
    let tickets_before = sqlite_state_writer_issued_tickets(&writer);
    let (tx, rx) = std::sync::mpsc::channel();
    state.persist_tx = tx;
    let error = state
        .prepare_engram_authority(&store, &binding, Duration::from_millis(10))
        .unwrap_err();
    assert!(
        error.message.contains("persistence is unconfirmed"),
        "{error:?}"
    );
    assert!(matches!(rx.try_recv().unwrap(), PersistRequest::Fence(_)));
    assert_eq!(sqlite_state_writer_issued_tickets(&writer), tickets_before);
    let inner = state.inner.lock().unwrap();
    assert!(engram_authority_work_unresolved(
        &inner,
        &store,
        &binding.work_id
    ));
    assert_eq!(
        inner.engram_work_naming_history[0]
            .transition
            .as_ref()
            .unwrap()
            .phase,
        EngramAuthorityPhase::Prepared
    );
}

#[test]
fn fixture_authority_budget_rejects_superseded_manual_images() {
    let (state, project, _, root) = fixture();
    install_store(&state, &project, &root);
    let store = established_store(&state, &project).unwrap();
    let binding = state.inner.lock().unwrap().engram_work_naming_history[0]
        .transition
        .as_ref()
        .unwrap()
        .binding
        .clone();
    let writer = sqlite_state_write_lock(state.persistence_path.as_path());
    let held = lock_sqlite_state_writer(&writer);
    let ticket = sqlite_state_writer_issued_tickets(&writer) + 1;
    let worker_state = state.clone();
    let worker_store = store.clone();
    let task = std::thread::spawn(move || {
        worker_state.prepare_engram_authority(&worker_store, &binding, Duration::from_secs(2))
    });
    wait_for_sqlite_state_writer_issued_tickets(&writer, ticket);
    state.inner.lock().unwrap().engram_work_naming_history[0]
        .transition
        .as_mut()
        .unwrap()
        .id
        .push_str("-superseded");
    drop(held);
    let error = task.join().unwrap().unwrap_err();
    assert!(
        error.message.contains("superseded authority image"),
        "{error:?}"
    );
    let inner = state.inner.lock().unwrap();
    assert!(engram_authority_work_unresolved(
        &inner,
        &store,
        &inner.engram_work_naming_history[0].work_id
    ));
}
