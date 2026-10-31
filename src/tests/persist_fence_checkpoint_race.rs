//! Fence settlement for the typed checkpoint mirror-versus-ledger race: a
//! refused cut keeps its fences, hooks and deadlines for the next coherent
//! tick, which acknowledges only targets it proves. Owns only these contracts;
//! the general fence coverage stays in persist_fence.rs, which mounts this
//! file. New file, not split from another.

use super::*;

/// The refusal the delta writer returns for the race, as it reaches the
/// fence batch: the typed mismatch under the cut's usual context.
fn checkpoint_mirror_race() -> anyhow::Error {
    anyhow::Error::new(PersistCheckpointMirrorLedgerMismatch)
        .context("refusing incoherent checkpoint persistence cut")
}

/// A wait registration that is neither in state nor durable, so no write can
/// prove it: it stays unmatched until its own deadline.
fn unmatched_wait_record(id: &str) -> DelegationWaitRecord {
    DelegationWaitRecord {
        id: id.to_owned(),
        parent_session_id: "parent".to_owned(),
        delegation_ids: vec!["child-delegation".to_owned()],
        mode: DelegationWaitMode::All,
        created_at: stamp_now(),
        title: None,
    }
}

/// Enqueues a fence whose acknowledgement hook counts its runs.
fn enqueue_counted(
    fixture: &FenceFixture,
    target: PersistFenceTarget,
    deadline: std::time::Instant,
) -> (PersistFenceWaiter, Arc<std::sync::atomic::AtomicUsize>) {
    let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&runs);
    let (fence, waiter) = PersistFence::new(target, deadline);
    let fence = fence.on_acknowledged(move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    });
    assert!(fixture.tx.send(PersistRequest::Fence(Box::new(fence))).is_ok());
    (waiter, runs)
}

fn runs(counter: &std::sync::atomic::AtomicUsize) -> usize {
    counter.load(std::sync::atomic::Ordering::Acquire)
}

#[test]
fn checkpoint_mirror_race_keeps_every_fence_and_deadline_until_exact_proof() {
    let mut fixture = FenceFixture::new();
    let deadline = std::time::Instant::now() + phase_sync::DEADLOCK_GUARD;
    let proven = PersistFenceTarget::WaitRegistration(fixture.wait_record("proven-after-race"));
    let unrelated =
        PersistFenceTarget::WaitRegistration(unmatched_wait_record("unmatched-after-race"));
    let (proven_waiter, proven_runs) = enqueue_counted(&fixture, proven, deadline);
    let (unrelated_waiter, unrelated_runs) = enqueue_counted(&fixture, unrelated, deadline);
    fixture.receive(false);
    fixture.batch.drain(&fixture.rx, false);
    let completions = fixture
        .batch
        .pending
        .iter()
        .map(|fence| Arc::clone(&fence.completion))
        .collect::<Vec<_>>();
    assert_eq!(completions.len(), 2);

    // Two refused ticks in a row: neither settles, extends or replaces a fence.
    for refusal in 0..2 {
        let delta = fixture.collect();
        let result =
            persist_delta_with_fences_using(&delta, &mut fixture.batch, || {
                Err(checkpoint_mirror_race())
            });
        assert!(result.is_err());
        assert_eq!(poll(&proven_waiter), None, "refusal {refusal} settled a fence");
        assert_eq!(poll(&unrelated_waiter), None, "refusal {refusal} settled a fence");
        assert_eq!(fixture.batch.pending.len(), 2);
        for (fence, completion) in fixture.batch.pending.iter().zip(&completions) {
            assert!(Arc::ptr_eq(&fence.completion, completion));
            assert_eq!(fence.completion.deadline, deadline);
        }
        assert_eq!((runs(&proven_runs), runs(&unrelated_runs)), (0, 0), "no hook on a refusal");
    }

    // The next coherent tick proves only what it wrote: the unrelated fence
    // stays pending within its deadline, and its hook does not run.
    let delta = fixture.collect();
    fixture.write(&delta).unwrap();
    assert_eq!(poll(&proven_waiter), Some(Ok(())));
    assert_eq!(poll(&unrelated_waiter), None, "a coherent write is not a blanket ACK");
    assert_eq!(fixture.batch.pending.len(), 1);
    assert_eq!((runs(&proven_runs), runs(&unrelated_runs)), (1, 0));
}

#[test]
fn checkpoint_mirror_race_keeps_the_original_deadline_and_never_resurrects_it() {
    let mut fixture = FenceFixture::new();
    let target = PersistFenceTarget::WaitRegistration(fixture.wait_record("expires-in-race"));
    // Already due: the retained fence keeps exactly this deadline.
    let (waiter, hook_runs) = enqueue_counted(&fixture, target, std::time::Instant::now());
    fixture.receive(false);
    fixture.batch.drain(&fixture.rx, false);
    let delta = fixture.collect();
    let refused = persist_delta_with_fences_using(&delta, &mut fixture.batch, || {
        Err(checkpoint_mirror_race())
    });
    assert!(refused.is_err());
    assert_eq!(poll(&waiter), Some(Err(PersistFenceError::Deadline)));
    let delta = fixture.collect();
    fixture.write(&delta).unwrap();
    assert_eq!(
        poll(&waiter),
        Some(Err(PersistFenceError::Deadline)),
        "a later coherent write cannot resurrect an expired fence"
    );
    assert_eq!(runs(&hook_runs), 0, "an expired fence never runs its hook");
}

#[test]
fn checkpoint_mirror_race_negatives_stay_terminal() {
    // Matching text without the type is an ordinary failure, as is every
    // other validation error under the same context.
    for error in [
        anyhow!(
            "refusing incoherent checkpoint persistence cut: \
             existing checkpoint mirror disagrees with its selected ledger entry"
        ),
        anyhow!("existing checkpoint mirror lacks its exact metadata protection")
            .context("refusing incoherent checkpoint persistence cut"),
    ] {
        let mut fixture = FenceFixture::new();
        let target = PersistFenceTarget::WaitRegistration(fixture.wait_record("ordinary-failure"));
        let deadline = std::time::Instant::now() + phase_sync::DEADLOCK_GUARD;
        let (waiter, hook_runs) = enqueue_counted(&fixture, target, deadline);
        fixture.receive(false);
        let delta = fixture.collect();
        let message = format!("{error:#}");
        let result = persist_delta_with_fences_using(&delta, &mut fixture.batch, || Err(error));
        assert!(result.is_err());
        assert_eq!(poll(&waiter), Some(Err(PersistFenceError::WriteFailed(message))));
        assert_eq!(runs(&hook_runs), 0);
    }

    // A fence retained by the race still ends, unpublished, on a genuine
    // write failure, the writer's shutdown, or its batch being dropped.
    for ending in ["genuine write failure", "shutdown", "drop"] {
        let mut fixture = FenceFixture::new();
        let target = PersistFenceTarget::WaitRegistration(fixture.wait_record("retained-ending"));
        let deadline = std::time::Instant::now() + phase_sync::DEADLOCK_GUARD;
        let (waiter, hook_runs) = enqueue_counted(&fixture, target, deadline);
        fixture.receive(false);
        let delta = fixture.collect();
        let refused = persist_delta_with_fences_using(&delta, &mut fixture.batch, || {
            Err(checkpoint_mirror_race())
        });
        assert!(refused.is_err());
        assert_eq!(poll(&waiter), None, "{ending}: the race retained the fence");
        let expected = match ending {
            "genuine write failure" => {
                let delta = fixture.collect();
                let failed = persist_delta_with_fences_using(&delta, &mut fixture.batch, || {
                    Err(anyhow!("injected transaction failure"))
                });
                assert!(failed.is_err());
                PersistFenceError::WriteFailed("injected transaction failure".to_owned())
            }
            "shutdown" => {
                fixture.batch.fail(PersistFenceError::Shutdown);
                PersistFenceError::Shutdown
            }
            _ => {
                drop(std::mem::take(&mut fixture.batch));
                PersistFenceError::WorkerStopped
            }
        };
        assert_eq!(poll(&waiter), Some(Err(expected)), "{ending}");
        assert_eq!(runs(&hook_runs), 0, "{ending}: a retained fence never publishes");
    }
}
