// Owns the tests of what a named-root authority fence reports when it does
// not confirm in time: a fence that misses its admission deadline names the
// persist phase it reached and whether its authority image is still owned;
// a fence whose image was superseded resolves at once instead of spending
// the remaining admission budget; a fence a tick leaves unmatched keeps that
// tick's phase timings; and the timing record covers every phase, a failed
// one included. The fence tests drive a gated persist writer, the deadline
// tests on a scripted budget clock; none applies real load. Does
// not own the generic fence batch tests (src/tests/persist_fence.rs) or the
// admission budget split before Begin (src/engram_host_adapter.rs). New file.
use super::*;

const BUDGET: Duration = Duration::from_secs(20);

struct GatedAuthority {
    state: AppState,
    clock: EngramBudgetClock,
    writer: mpsc::Receiver<PersistRequest>,
    store: EngramAuthorityStoreKey,
    binding: EngramControlWorkBinding,
}

impl GatedAuthority {
    fn new(label: &str) -> Self {
        let mut state = test_app_state();
        let clock = state.select_test_scripted_engram_budget_clock();
        let (tx, writer) = mpsc::channel();
        state.persist_tx = tx;
        Self {
            state,
            clock,
            writer,
            store: EngramAuthorityStoreKey {
                database_path: PathBuf::from(format!("{label}-engram.db")),
                project_id: format!("github.com/example/{label}"),
            },
            binding: EngramControlWorkBinding {
                root_execution_id: format!("root-{label}"),
                work_id: format!("work-{label}"),
                run_id: format!("run-{label}"),
                work_revision: 1,
                claim_id: format!("claim-{label}"),
                claim_fence: 10,
            },
        }
    }

    /// Prepares the work's next owner image, superseding any earlier one.
    fn prepare(&self) -> EngramAuthorityImage {
        let mut inner = self.state.inner.lock().expect("state mutex poisoned");
        self.state
            .prepare_engram_authority_image_locked(&mut inner, &self.store, &self.binding)
            .expect("the owner image is prepared")
    }

    /// Confirms `image` on its own thread, as the guard before Begin does.
    fn confirm(
        &self,
        image: EngramAuthorityImage,
    ) -> mpsc::Receiver<std::result::Result<(), ApiError>> {
        let deadline = self.clock.now() + BUDGET;
        let state = self.state.clone();
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = done_tx.send(state.confirm_engram_authority_image_until(&image, deadline));
        });
        done_rx
    }

    /// The fence the confirmation sent, held back from the writer.
    fn sent_fence(&self) -> Box<PersistFence> {
        loop {
            if let PersistRequest::Fence(fence) = self
                .writer
                .recv_timeout(crate::TEST_PHASE_DEADLOCK_GUARD)
                .expect("the confirmation must send its fence")
            {
                return fence;
            }
        }
    }

    /// Runs out the whole admission budget once the waiter is parked.
    fn spend_budget(&self) {
        self.clock.wait_for_scripted_waiter();
        self.clock.advance(BUDGET);
    }

    fn tick(&self, batch: &mut PersistFenceBatch) {
        let delta = collect_persist_delta_from_shared_state(&self.state.inner, 0);
        let mut cache = SqlitePersistConnectionCache::new();
        persist_delta_with_fences(
            &mut cache,
            self.state.persistence_path.as_path(),
            &delta,
            batch,
        )
        .expect("the gated tick commits");
    }
}

fn refusal(done: &mpsc::Receiver<std::result::Result<(), ApiError>>) -> ApiError {
    done.recv_timeout(crate::TEST_PHASE_DEADLOCK_GUARD)
        .expect("the confirmation must finish")
        .expect_err("an unconfirmed fence must refuse")
}

#[test]
fn authority_fence_deadline_while_queued_names_the_phase_and_ownership() {
    let gated = GatedAuthority::new("fence-phase-queued");
    let done = gated.confirm(gated.prepare());
    // The writer is busy elsewhere: the fence stays queued in its channel.
    let held = gated.sent_fence();
    gated.spend_budget();
    let error = refusal(&done);
    drop(held);
    assert!(
        error.message.contains("queued for the writer")
            && error.message.contains("image is still owned"),
        "a deadline must name the queued phase and the image's ownership: {}",
        error.message
    );
}

#[test]
fn authority_fence_deadline_after_acceptance_names_the_awaited_tick() {
    let gated = GatedAuthority::new("fence-phase-accepted");
    let done = gated.confirm(gated.prepare());
    let mut batch = PersistFenceBatch::default();
    // The writer took the fence but its tick has not started.
    batch.accept(PersistRequest::Fence(gated.sent_fence()));
    gated.spend_budget();
    let error = refusal(&done);
    drop(batch);
    assert!(
        error.message.contains("accepted, awaiting a writer tick")
            && error.message.contains("image is still owned"),
        "a deadline must name the awaited tick and the image's ownership: {}",
        error.message
    );
}

#[test]
fn authority_fence_deadline_report_never_waits_for_busy_state() {
    let gated = GatedAuthority::new("fence-phase-state-busy");
    let done = gated.confirm(gated.prepare());
    let held = gated.sent_fence();
    // Another operation holds the state across the deadline.
    let busy = gated.state.inner.lock().expect("state mutex poisoned");
    gated.spend_budget();
    let refused = done.recv_timeout(crate::TEST_PHASE_DEADLOCK_GUARD);
    drop(busy);
    drop(held);
    let error = refused
        .expect("the refusal must not wait for the busy state")
        .expect_err("an unconfirmed fence must refuse");
    assert!(
        error
            .message
            .contains("the authority image is unknown (state busy)"),
        "busy state must report ownership as unknown: {}",
        error.message
    );
}

#[test]
fn authority_fence_deadline_inside_a_tick_names_the_tick_and_its_age() {
    let gated = GatedAuthority::new("fence-phase-in-tick");
    let done = gated.confirm(gated.prepare());
    let mut batch = PersistFenceBatch::default();
    batch.accept(PersistRequest::Fence(gated.sent_fence()));
    // The writer's tick has started and is still collecting or committing.
    batch.mark_in_tick();
    gated.spend_budget();
    let error = refusal(&done);
    drop(batch);
    assert!(
        error.message.contains("while in a writer tick for ")
            && error
                .message
                .contains(" ms; the authority image is still owned"),
        "a deadline inside a tick must name the tick and how long it has run: {}",
        error.message
    );
}

#[test]
fn superseded_authority_fence_resolves_without_spending_the_budget() {
    let gated = GatedAuthority::new("fence-superseded");
    let started = gated.clock.now();
    let done = gated.confirm(gated.prepare());
    let mut batch = PersistFenceBatch::default();
    batch.accept(PersistRequest::Fence(gated.sent_fence()));
    // A later owner replaces the image before the writer's tick collects it.
    gated.prepare();
    gated.tick(&mut batch);
    // Decided by the tick itself, before any budget is spent.
    let resolved_by_the_tick = !batch.has_pending();
    let unspent = gated.clock.now() == started;
    if !resolved_by_the_tick {
        // Release the waiter through its deadline, so this test fails with
        // its message instead of a stranded thread and the clock watchdog.
        gated.spend_budget();
    }
    let error = refusal(&done);
    assert!(
        resolved_by_the_tick && unspent,
        "the tick that collects a later owner must resolve the fence before any budget \
         is spent: {}",
        error.message
    );
    assert!(
        error.message.contains("superseded authority image"),
        "a superseded image must resolve as superseded, not wait out its deadline: {}",
        error.message
    );
}

#[test]
fn an_unmatched_fence_keeps_the_phase_timings_of_its_tick() {
    let state = test_app_state();
    // Never part of any delta, so every tick leaves it pending.
    let (fence, waiter) = PersistFence::new(
        PersistFenceTarget::TestRunCardsEpoch("never-written".to_owned()),
        std::time::Instant::now() + crate::TEST_PHASE_DEADLOCK_GUARD,
    );
    assert_eq!(waiter.progress().phase, PersistFencePhase::Queued);
    let mut batch = PersistFenceBatch::default();
    batch.accept(PersistRequest::Fence(Box::new(fence)));
    assert_eq!(waiter.progress().phase, PersistFencePhase::Accepted);
    let delta = collect_persist_delta_from_shared_state(&state.inner, 0);
    let mut cache = SqlitePersistConnectionCache::new();
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &delta,
        &mut batch,
    )
    .expect("the tick commits");
    let progress = waiter.progress();
    assert_eq!(progress.phase, PersistFencePhase::WrittenUnmatched);
    let described = progress.describe();
    assert!(
        progress.last_tick.is_some()
            && described.starts_with("written but unmatched; last tick")
            && described.contains("commit"),
        "an unmatched fence must keep its tick's phase timings: {described}"
    );
}

#[test]
fn tick_timings_total_and_summary_cover_every_phase() {
    let ms = Duration::from_millis;
    let timings = PersistTickTimings {
        collect: ms(1),
        serialize: ms(2),
        open: ms(3),
        ticket_wait: ms(4),
        statements: ms(5),
        commit: ms(6),
        post_commit: ms(7),
        acknowledge: ms(8),
    };
    assert_eq!(timings.total(), ms(36));
    assert_eq!(
        timings.summary(),
        "collect 1 ms, serialize 2 ms, open 3 ms, writer ticket 4 ms, statements 5 ms, \
         commit 6 ms, post-commit 7 ms, acknowledge 8 ms"
    );
}

#[test]
fn a_failed_write_phase_still_records_its_time() {
    let root = TestTempRoot::create("persist-failed-phase");
    // A regular file where the state directory belongs makes opening fail.
    let blocker = root.path().join("not-a-directory");
    std::fs::write(&blocker, b"blocks the state directory").expect("blocker file");
    let unset = Duration::from_secs(9_999);
    let mut timings = PersistTickTimings {
        open: unset,
        ..PersistTickTimings::default()
    };
    let delta = collect_persist_delta_from_shared_state(&test_app_state().inner, 0);
    let mut cache = SqlitePersistConnectionCache::new();
    let error = persist_delta_via_cache_timed(
        &mut cache,
        &blocker.join("termal.sqlite"),
        &delta,
        &mut timings,
    )
    .expect_err("opening state under a regular file must fail");
    assert_ne!(
        timings.open, unset,
        "the failed open phase must still record its time: {error:#}"
    );
}
