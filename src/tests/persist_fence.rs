//! Content-fence persistence contracts on a connected receiver and temp SQLite.
//! New coverage beside persist.rs; no delegation admission, HTTP, or provider
//! fixtures live here. The worker steps are driven explicitly, never by sleeps.

use super::*;

struct FenceFixture {
    cache: SqlitePersistConnectionCache,
    root: TestTempRoot,
    inner: Arc<StateMutex<StateInner>>,
    tx: mpsc::Sender<PersistRequest>,
    rx: mpsc::Receiver<PersistRequest>,
    batch: PersistFenceBatch,
}

impl FenceFixture {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            cache: SqlitePersistConnectionCache::new(),
            root: TestTempRoot::create("persist-fence"),
            inner: Arc::new(StateMutex::new(StateInner::new())),
            tx,
            rx,
            batch: PersistFenceBatch::default(),
        }
    }

    fn wait_record(&self, id: &str) -> DelegationWaitRecord {
        let record = DelegationWaitRecord {
            id: id.to_owned(),
            parent_session_id: "parent".to_owned(),
            delegation_ids: vec!["child-delegation".to_owned()],
            mode: DelegationWaitMode::All,
            created_at: stamp_now(),
            title: None,
        };
        self.inner
            .lock()
            .unwrap()
            .delegation_waits
            .push(record.clone());
        record
    }

    fn enqueue(&self, target: PersistFenceTarget) -> PersistFenceWaiter {
        let (fence, waiter) = PersistFence::new(
            target,
            std::time::Instant::now() + phase_sync::DEADLOCK_GUARD,
        );
        assert!(self.tx.send(PersistRequest::Fence(Box::new(fence))).is_ok());
        waiter
    }

    fn receive(&mut self, retry: bool) -> PersistWorkerWaitOutcome {
        PersistWorkerRetryState {
            retry_after_failure: retry,
            retry_delay: phase_sync::DEADLOCK_GUARD,
            ..PersistWorkerRetryState::default()
        }
        .wait_for_next_tick(&self.rx, &mut self.batch)
    }

    fn collect(&self) -> PersistDelta {
        collect_persist_delta_from_shared_state(&self.inner, 0)
    }

    fn write(&mut self, delta: &PersistDelta) -> Result<Vec<String>> {
        persist_delta_with_fences(
            &mut self.cache,
            &self.root.path().join("termal.sqlite"),
            delta,
            &mut self.batch,
        )
    }
}

fn poll(waiter: &PersistFenceWaiter) -> Option<PersistFenceResult> {
    waiter.completion.poll_at(std::time::Instant::now())
}

#[test]
fn held_fence_keeps_the_state_lock_free_and_completes_only_after_sql_write() {
    let mut fixture = FenceFixture::new();
    let expected = fixture.wait_record("held");
    let waiter = fixture.enqueue(PersistFenceTarget::WaitRegistration(expected));
    assert_eq!(fixture.receive(false), PersistWorkerWaitOutcome::Process);
    assert_eq!(poll(&waiter), None);
    // The actual waiter runs on another thread while this thread takes the
    // state lock and commits. Scoped join plus the fence deadline bounds unwind.
    std::thread::scope(|scope| {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let handle = scope.spawn(move || {
            entered_tx.send(()).unwrap();
            done_tx.send(waiter.wait()).unwrap();
        });
        phase_sync::receive(&entered_rx, "fence waiter entered");
        fixture.inner.lock().unwrap().revision += 1;
        assert!(matches!(done_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        let delta = fixture.collect();
        fixture.write(&delta).unwrap();
        // Assert before a blocking receive so an absent ack fails immediately;
        // dropping the batch releases the waiter even on the RED path.
        let proof = fixture
            .batch
            .pending
            .first()
            .map(|fence| fence.completion.poll_at(std::time::Instant::now()));
        fixture.batch.fail(PersistFenceError::WorkerStopped);
        let result = phase_sync::receive(&done_rx, "fence receipt after SQL commit");
        handle.join().unwrap();
        assert_eq!(
            result,
            Ok(()),
            "SQL success must resolve the content fence: {proof:?}"
        );
    });
}

#[test]
fn write_failure_is_reported_to_the_fence_not_only_the_worker() {
    let mut fixture = FenceFixture::new();
    let expected = fixture.wait_record("failed-write");
    let waiter = fixture.enqueue(PersistFenceTarget::WaitRegistration(expected));
    fixture.receive(false);
    let delta = fixture.collect();
    let result = persist_delta_with_fences_using(&delta, &mut fixture.batch, || {
        Err(anyhow!("injected transaction failure"))
    });
    assert!(result.is_err());
    assert_eq!(
        poll(&waiter),
        Some(Err(PersistFenceError::WriteFailed(
            "injected transaction failure".to_owned()
        )))
    );
}

#[test]
fn acknowledgement_waits_for_post_commit_integrity_and_preserves_its_failure() {
    let mut fixture = FenceFixture::new();
    let expected = fixture.wait_record("post-commit");
    let waiter = fixture.enqueue(PersistFenceTarget::WaitRegistration(expected));
    fixture.receive(false);
    let delta = fixture.collect();
    let path = fixture.root.path().join("termal.sqlite");
    let result = persist_delta_with_fences_using(&delta, &mut fixture.batch, || {
        assert_eq!(poll(&waiter), None, "no pre-write success");
        persist_delta_via_cache(&mut fixture.cache, &path, &delta)?;
        assert_eq!(poll(&waiter), None, "no success before integrity outcome");
        Err(anyhow!("injected post-commit integrity failure"))
    });
    assert!(result.is_err());
    assert_eq!(
        poll(&waiter),
        Some(Err(PersistFenceError::WriteFailed(
            "injected post-commit integrity failure".to_owned()
        )))
    );
}

#[test]
fn a_deadline_is_terminal_even_if_the_commit_acknowledgement_arrives_late() {
    let mut fixture = FenceFixture::new();
    let expected = fixture.wait_record("deadline");
    let waiter = fixture.enqueue(PersistFenceTarget::WaitRegistration(expected));
    fixture.receive(false);
    // Advance the completion's clock explicitly, not the wall clock. This is
    // the deadline/COMMIT race oracle; no sleep is involved.
    assert_eq!(
        waiter.completion.poll_at(waiter.completion.deadline),
        Some(Err(PersistFenceError::Deadline))
    );
    let delta = fixture.collect();
    fixture.write(&delta).unwrap();
    assert_eq!(waiter.wait(), Err(PersistFenceError::Deadline));
}

fn delegation(id: &str) -> DelegationRecord {
    DelegationRecord {
        id: id.to_owned(),
        parent_session_id: "parent".to_owned(),
        child_session_id: "child".to_owned(),
        mode: DelegationMode::Reviewer,
        status: DelegationStatus::Running,
        title: "Fence".to_owned(),
        prompt: "review".to_owned(),
        cwd: "/tmp".to_owned(),
        agent: Agent::Codex,
        model: None,
        write_policy: DelegationWritePolicy::ReadOnly,
        created_at: stamp_now(),
        started_at: None,
        completed_at: None,
        result: None,
        submitted_review_result: None,
        post_submission_transport_error: None,
        review_result_recovery_probe_attempt: None,
        review_result_recovery_error: None,
        review_result_schema_version: None,
        review_result_submission_attempt: 2,
    }
}

#[test]
fn split_lock_omission_and_newer_revision_do_not_prove_the_target_was_written() {
    let mut fixture = FenceFixture::new();
    let expected = delegation("deferred");
    {
        let mut inner = fixture.inner.lock().unwrap();
        inner.delegations.push(expected.clone());
        inner.mark_delegation_mutated(0);
    }
    let waiter = fixture.enqueue(PersistFenceTarget::Delegation(Box::new(expected.clone())));
    fixture.receive(false);
    // Use the real split-lock one-pass seam to force omission. Both passes of
    // production collection use this same materialization contract.
    let delta =
        collect_persist_delta_pass_from_shared_state(&fixture.inner, 0, &BTreeSet::new(), || {
            let mut inner = fixture.inner.lock().unwrap();
            inner.mark_delegation_mutated(0);
            inner.revision += 100;
        });
    assert!(
        delta
            .changed_delegations
            .as_deref()
            .unwrap_or_default()
            .is_empty()
    );
    fixture.write(&delta).unwrap();
    assert_eq!(
        poll(&waiter),
        None,
        "watermark/revision alone cannot acknowledge"
    );
    assert!(
        fixture.batch.has_pending(),
        "deferred target remains a retry demand"
    );
    let delta = fixture.collect();
    fixture.write(&delta).unwrap();
    assert_eq!(poll(&waiter), Some(Ok(())));
}

#[test]
fn changed_content_with_the_same_id_cannot_satisfy_a_fence() {
    let mut fixture = FenceFixture::new();
    let expected = fixture.wait_record("same-id");
    let waiter = fixture.enqueue(PersistFenceTarget::WaitRegistration(expected.clone()));
    fixture.receive(false);
    fixture.inner.lock().unwrap().delegation_waits[0].title = Some("different".to_owned());
    let delta = fixture.collect();
    fixture.write(&delta).unwrap();
    assert_eq!(poll(&waiter), None);
    fixture.inner.lock().unwrap().delegation_waits[0] = expected;
    let delta = fixture.collect();
    fixture.write(&delta).unwrap();
    assert_eq!(poll(&waiter), Some(Ok(())));
}

#[test]
fn delta_fence_fence_shutdown_drain_preserves_both_acknowledgements() {
    for retry in [false, true] {
        let mut fixture = FenceFixture::new();
        assert!(fixture.tx.send(PersistRequest::Delta).is_ok());
        let first = fixture.wait_record("first");
        let first = fixture.enqueue(PersistFenceTarget::WaitRegistration(first));
        assert!(fixture.tx.send(PersistRequest::Delta).is_ok());
        let second = fixture.wait_record("second");
        let second = fixture.enqueue(PersistFenceTarget::WaitRegistration(second));
        assert!(fixture.tx.send(PersistRequest::Shutdown).is_ok());
        assert_eq!(fixture.receive(retry), PersistWorkerWaitOutcome::Process);
        assert!(fixture.batch.drain(&fixture.rx, false));
        assert_eq!(fixture.batch.pending.len(), 2);
        assert_eq!(poll(&first), None);
        assert_eq!(poll(&second), None);
        let delta = fixture.collect();
        fixture.write(&delta).unwrap();
        assert_eq!(poll(&first), Some(Ok(())));
        assert_eq!(poll(&second), Some(Ok(())));
    }
}

#[test]
fn shutdown_and_disconnection_resolve_unproven_waiters_explicitly() {
    let mut fixture = FenceFixture::new();
    let expected = fixture.wait_record("shutdown");
    let waiter = fixture.enqueue(PersistFenceTarget::WaitRegistration(expected));
    fixture.receive(false);
    fixture.batch.fail(PersistFenceError::Shutdown);
    assert_eq!(waiter.wait(), Err(PersistFenceError::Shutdown));

    let expected = fixture.wait_record("disconnect");
    let waiter = fixture.enqueue(PersistFenceTarget::WaitRegistration(expected));
    drop(fixture.rx);
    assert_eq!(waiter.wait(), Err(PersistFenceError::WorkerStopped));
}

#[test]
fn late_fence_reads_already_durable_content_without_rewriting_the_delegation() {
    let mut fixture = FenceFixture::new();
    let expected = delegation("late-arrival");
    {
        let mut inner = fixture.inner.lock().unwrap();
        inner.delegations.push(expected.clone());
        inner.mark_delegation_mutated(0);
    }
    assert!(fixture.tx.send(PersistRequest::Delta).is_ok());
    fixture.receive(false);
    assert!(!fixture.batch.drain(&fixture.rx, false));
    let delta = fixture.collect();
    let watermark = delta.watermark;
    let (fence, waiter) = PersistFence::new(
        PersistFenceTarget::Delegation(Box::new(expected)),
        std::time::Instant::now() + phase_sync::DEADLOCK_GUARD,
    );
    // Drive the write phase explicitly after drain, with the fence arriving
    // inside it. Its receiver cannot see this request until the next tick.
    persist_delta_with_fences_using(&delta, &mut fixture.batch, || {
        assert!(
            fixture
                .tx
                .send(PersistRequest::Fence(Box::new(fence)))
                .is_ok()
        );
        persist_delta_via_cache(
            &mut fixture.cache,
            &fixture.root.path().join("termal.sqlite"),
            &delta,
        )
    })
    .unwrap();
    assert_eq!(poll(&waiter), None);
    fixture.receive(false);
    let next = collect_persist_delta_from_shared_state(&fixture.inner, watermark);
    assert!(
        next.changed_delegations
            .as_deref()
            .unwrap_or_default()
            .is_empty()
    );
    // A trigger turns any redundant delegation write into a test failure.
    fixture
        .cache
        .connection
        .as_ref()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER reject_redundant_delegation_write BEFORE UPDATE ON delegations
         BEGIN SELECT RAISE(ABORT, 'delegation must not be rewritten'); END;",
        )
        .unwrap();
    fixture.write(&next).unwrap();
    assert_eq!(
        poll(&waiter),
        Some(Ok(())),
        "late fence needs committed content proof"
    );
}

#[test]
fn cached_readback_requires_exact_content_not_just_the_bound_id() {
    let mut fixture = FenceFixture::new();
    let stored = delegation("different-durable-content");
    {
        let mut inner = fixture.inner.lock().unwrap();
        inner.delegations.push(stored.clone());
        inner.mark_delegation_mutated(0);
    }
    let delta = fixture.collect();
    fixture.write(&delta).unwrap();
    let mut requested = stored.clone();
    requested.prompt = "not the durable prompt".to_owned();
    let waiter = fixture.enqueue(PersistFenceTarget::Delegation(Box::new(requested)));
    fixture.receive(false);
    let empty = collect_persist_delta_from_shared_state(&fixture.inner, delta.watermark);
    fixture.write(&empty).unwrap();
    assert_eq!(
        poll(&waiter),
        None,
        "row existence cannot prove different content"
    );
    fixture.batch.fail(PersistFenceError::Shutdown);
    assert_eq!(waiter.wait(), Err(PersistFenceError::Shutdown));

    let waiter = fixture.enqueue(PersistFenceTarget::Delegation(Box::new(stored)));
    fixture.receive(false);
    fixture.write(&empty).unwrap();
    assert_eq!(
        poll(&waiter),
        Some(Ok(())),
        "exact durable content does prove it"
    );
}

#[test]
fn real_sql_failure_completes_fences_and_preserves_the_writer_error() {
    let mut fixture = FenceFixture::new();
    let delta = fixture.collect();
    fixture.write(&delta).unwrap();
    fixture
        .cache
        .connection
        .as_ref()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER fail_metadata_write BEFORE UPDATE ON app_state
         BEGIN SELECT RAISE(ABORT, 'injected-fence-write-failure'); END;",
        )
        .unwrap();
    let expected = fixture.wait_record("sql-error");
    let waiter = fixture.enqueue(PersistFenceTarget::WaitRegistration(expected));
    fixture.receive(false);
    let delta = fixture.collect();
    let error = fixture.write(&delta).unwrap_err();
    assert!(format!("{error:#}").contains("injected-fence-write-failure"));
    assert_eq!(
        poll(&waiter),
        Some(Err(PersistFenceError::WriteFailed(format!("{error:#}"))))
    );
    assert!(
        fixture.cache.connection.is_none(),
        "existing invalidation policy remains intact"
    );
}

#[test]
fn pending_fences_keep_worker_retry_alive_and_shutdown_resolves_them() {
    for retry in [false, true] {
        let mut fixture = FenceFixture::new();
        let expected = fixture.wait_record("not-materialized");
        let waiter = fixture.enqueue(PersistFenceTarget::WaitRegistration(expected));
        assert_eq!(fixture.receive(retry), PersistWorkerWaitOutcome::Process);
        assert_eq!(
            fixture.batch.pending.len(),
            1,
            "first recv must preserve the fence"
        );
        fixture.inner.lock().unwrap().delegation_waits.clear();
        let delta = fixture.collect();
        fixture.write(&delta).unwrap();
        let mut state = PersistWorkerRetryState::default();
        assert!(!state.finish_fenced_tick(&Ok(()), false, &mut fixture.batch));
        assert!(
            state.next_tick_delay().is_some(),
            "unproven fence needs another tick without a wake"
        );
        assert!(
            !state.retry_after_failure,
            "successful SQL is not a write failure"
        );
        assert_eq!(poll(&waiter), None);
        assert!(state.finish_fenced_tick(&Ok(()), true, &mut fixture.batch));
        assert_eq!(waiter.wait(), Err(PersistFenceError::Shutdown));
        assert!(!fixture.batch.has_pending());
    }
}

#[test]
fn expired_fence_is_not_resurrected_by_a_later_worker_success() {
    let mut fixture = FenceFixture::new();
    let expected = fixture.wait_record("expired-before-receive");
    let (fence, waiter) = PersistFence::new(
        PersistFenceTarget::WaitRegistration(expected),
        std::time::Instant::now(),
    );
    assert!(
        fixture
            .tx
            .send(PersistRequest::Fence(Box::new(fence)))
            .is_ok()
    );
    fixture.receive(false);
    assert_eq!(
        waiter.completion.poll_at(waiter.completion.deadline),
        Some(Err(PersistFenceError::Deadline))
    );
    let delta = fixture.collect();
    fixture.write(&delta).unwrap();
    let mut retry = PersistWorkerRetryState::default();
    assert!(!retry.finish_fenced_tick(&Ok(()), false, &mut fixture.batch));
    assert!(
        retry.next_tick_delay().is_none(),
        "expired request must not keep the worker retrying"
    );
    assert_eq!(waiter.wait(), Err(PersistFenceError::Deadline));
}

#[test]
fn unproductive_fence_retries_grow_to_a_cap_without_resetting_on_successful_sql() {
    let mut fixture = FenceFixture::new();
    let target = fixture.wait_record("absent-target");
    let waiter = fixture.enqueue(PersistFenceTarget::WaitRegistration(target));
    fixture.receive(false);
    fixture.inner.lock().unwrap().delegation_waits.clear();
    let delta = fixture.collect();
    let mut retry = PersistWorkerRetryState::default();
    let (phase_tx, phase_rx) = mpsc::channel();
    for milliseconds in [250, 500, 1000, 2000, 4000, 8000, 16000, 30000, 30000] {
        fixture.write(&delta).unwrap();
        assert!(!retry.finish_fenced_tick(&Ok(()), false, &mut fixture.batch));
        // Replace only the channel's timeout boundary. The production scheduler
        // chooses the delay and sees an explicit timeout event; no wall-clock
        // sleep or elapsed-time assertion is the oracle.
        assert_eq!(
            retry.wait_for_next_tick_using(&mut fixture.batch, |delay| {
                phase_tx.send(delay).unwrap();
                Err(mpsc::RecvTimeoutError::Timeout)
            }),
            PersistWorkerWaitOutcome::Process
        );
        assert_eq!(
            phase_sync::receive(&phase_rx, "fence retry timeout scheduled"),
            Some(Duration::from_millis(milliseconds))
        );
        assert_eq!(poll(&waiter), None);
    }
    fixture.batch.fail(PersistFenceError::Shutdown);
}

#[test]
fn fence_backoff_is_independent_and_a_new_fence_wakes_and_resets_it() {
    let mut fixture = FenceFixture::new();
    let target = fixture.wait_record("new-durable-target");
    let mut retry = PersistWorkerRetryState {
        fence_retry_delay: Some(PERSIST_FENCE_RETRY_MAX_DELAY),
        ..PersistWorkerRetryState::default()
    };
    retry.record_result(&Err(anyhow!("injected write failure")));
    assert_eq!(retry.next_tick_delay(), Some(Duration::from_millis(500)));
    assert_eq!(retry.fence_retry_delay, Some(PERSIST_FENCE_RETRY_MAX_DELAY));
    retry.record_result(&Ok(()));
    assert!(!retry.retry_after_failure);
    assert_eq!(retry.retry_delay, PERSIST_RETRY_SEED_DELAY);
    assert_eq!(retry.next_tick_delay(), Some(PERSIST_FENCE_RETRY_MAX_DELAY));

    let waiter = fixture.enqueue(PersistFenceTarget::WaitRegistration(target));
    assert_eq!(
        retry.wait_for_next_tick_using(&mut fixture.batch, |delay| {
            assert_eq!(delay, Some(PERSIST_FENCE_RETRY_MAX_DELAY));
            // A queued channel message wins over the scheduled timeout. Do not
            // actually wait thirty seconds if the fixture fails to enqueue it.
            Ok(fixture
                .rx
                .try_recv()
                .expect("new fence is ready during backoff"))
        }),
        PersistWorkerWaitOutcome::Process
    );
    let delta = fixture.collect();
    fixture.write(&delta).unwrap();
    assert!(!retry.finish_fenced_tick(&Ok(()), false, &mut fixture.batch));
    assert_eq!(poll(&waiter), Some(Ok(())));
    assert_eq!(retry.next_tick_delay(), None);

    let absent = fixture.wait_record("next-absent-target");
    let next_waiter = fixture.enqueue(PersistFenceTarget::WaitRegistration(absent));
    fixture.receive(false);
    fixture.inner.lock().unwrap().delegation_waits.clear();
    let next_delta = fixture.collect();
    fixture.write(&next_delta).unwrap();
    assert!(!retry.finish_fenced_tick(&Ok(()), false, &mut fixture.batch));
    assert_eq!(
        retry.next_tick_delay(),
        Some(PERSIST_FENCE_RETRY_SEED_DELAY)
    );
    fixture.batch.fail(PersistFenceError::Shutdown);
    assert_eq!(next_waiter.wait(), Err(PersistFenceError::Shutdown));
}

#[test]
fn a_late_wait_registration_is_proven_by_metadata_without_a_new_mutation() {
    let mut fixture = FenceFixture::new();
    let expected = fixture.wait_record("late-wait");
    assert!(fixture.tx.send(PersistRequest::Delta).is_ok());
    fixture.receive(false);
    fixture.batch.drain(&fixture.rx, false);
    let delta = fixture.collect();
    let before = {
        let state = fixture.inner.lock().unwrap();
        (state.revision, state.last_mutation_stamp)
    };
    let (fence, waiter) = PersistFence::new(
        PersistFenceTarget::WaitRegistration(expected.clone()),
        std::time::Instant::now() + phase_sync::DEADLOCK_GUARD,
    );
    persist_delta_with_fences_using(&delta, &mut fixture.batch, || {
        assert!(
            fixture
                .tx
                .send(PersistRequest::Fence(Box::new(fence)))
                .is_ok()
        );
        persist_delta_via_cache(
            &mut fixture.cache,
            &fixture.root.path().join("termal.sqlite"),
            &delta,
        )
    })
    .unwrap();
    assert_eq!(poll(&waiter), None);
    fixture.receive(false);
    let next = collect_persist_delta_from_shared_state(&fixture.inner, delta.watermark);
    assert!(next.changed_sessions.is_empty());
    assert!(
        next.changed_delegations
            .as_deref()
            .unwrap_or_default()
            .is_empty()
    );
    assert!(next.metadata.delegation_waits.contains(&expected));
    fixture.write(&next).unwrap();
    let after = {
        let state = fixture.inner.lock().unwrap();
        (state.revision, state.last_mutation_stamp)
    };
    assert_eq!(
        before, after,
        "no mutation was needed to prove registration"
    );
    assert_eq!(poll(&waiter), Some(Ok(())));
}
