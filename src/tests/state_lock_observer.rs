// Deterministic observer schedules and integer-clock boundaries. No load or
// wall-time performance assertion; production mutexes still own every guard.
use super::*;

#[track_caller]
fn span(
    id: u64,
    epoch: u64,
    requested: u64,
    acquired: u64,
    released: Option<u64>,
) -> StateLockSpan {
    StateLockSpan {
        id,
        epoch,
        requested,
        acquired,
        released,
        site: std::panic::Location::caller(),
    }
}

fn waits(wait: StateLockSpan) -> [Option<StateLockSpan>; 8] {
    let mut rows = [None; 8];
    rows[0] = Some(wait);
    rows
}

#[test]
fn review_observer_first_eviction_counters_are_inside_snapshot() {
    let observer = Arc::new(StateLockObserver::new("instance-one"));
    observer.released(span(1, 1, 0, 10, Some(30)));
    for id in 2..=256 {
        observer.released(span(id, 1, 50, 50, Some(60)));
    }
    let writer = Arc::clone(&observer);
    TEST_STATE_LOCK_COUNTERS_SAMPLED.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            // Evict the only overlapping holder after counters were sampled.
            writer.released(span(257, 1, 60, 60, Some(70)));
        }));
    });
    let block = StateLockBlock::collect_at(
        &observer,
        &waits(span(1000, 1, 20, 40, None)),
        0,
        100,
    );
    assert!(block.holders.is_empty(), "fixture evicted its sole overlap");
    assert!(
        block.partial && (block.unavailable || block.eviction_count > 0),
        "a snapshot cannot pair post-eviction history with zero loss evidence: {block:?}"
    );
}

#[test]
fn review_observer_holder_cap_keeps_active_and_longest_overlap() {
    let observer = StateLockObserver::new("instance-one");
    let origin = *STATE_LOCK_ORIGIN;
    let active = observer.acquired(1, std::panic::Location::caller(), origin, origin);
    for offset in 1..=255 {
        let row = if offset <= 5 {
            span(active.id + offset, 1, 10, 10 + offset, Some(20 + offset))
        } else {
            span(active.id + offset, 1, 1100, 1100, Some(1200))
        };
        observer.released(row);
    }
    let long_id = active.id + 256;
    observer.released(span(long_id, 1, 0, 0, Some(900)));
    let block = StateLockBlock::collect_at(
        &observer,
        &waits(span(active.id + 1000, 1, 0, 1000, None)),
        0,
        2000,
    );
    assert_eq!(block.holders.len(), 4);
    assert!(block.partial && block.truncation_count > 0);
    assert_eq!(
        block.holders[0].hold_id,
        active.id.to_string(),
        "the overlapping active holder must precede released history"
    );
    assert!(
        block
            .holders
            .iter()
            .any(|row| row.hold_id == long_id.to_string()),
        "the late-slot longest overlap must survive the holder cap: {block:?}"
    );
}

#[test]
fn overlap_excludes_self_unrelated_late_and_cross_epoch_spans() {
    let wait = span(100, 7, 20, 60, None);
    assert!(state_lock_overlap(span(1, 7, 0, 10, Some(21)), wait, 100));
    assert!(state_lock_overlap(span(2, 7, 0, 30, Some(40)), wait, 100));
    assert!(!state_lock_overlap(span(3, 7, 0, 10, Some(20)), wait, 100));
    assert!(!state_lock_overlap(span(4, 7, 0, 60, Some(80)), wait, 100));
    assert!(!state_lock_overlap(span(5, 8, 0, 10, Some(80)), wait, 100));
    assert!(!state_lock_overlap(wait, wait, 100));
    let observer = StateLockObserver::new("instance-one");
    for row in [
        span(1, 7, 0, 10, Some(21)),
        span(2, 7, 0, 30, Some(40)),
        span(3, 7, 0, 60, Some(80)),
        span(4, 8, 0, 10, Some(80)),
    ] {
        observer.released(row);
    }
    let block = StateLockBlock::collect(&observer, &waits(wait), 0);
    assert_eq!(
        block
            .holders
            .iter()
            .map(|row| row.hold_id.as_str())
            .collect::<Vec<_>>(),
        ["2", "1"]
    );
    assert!(block.partial && block.truncation_count == 1);
}

#[test]
fn late_release_cas_cannot_clear_successor_even_when_ring_append_is_lost() {
    use std::sync::atomic::Ordering::SeqCst;
    let observer = StateLockObserver::new("instance-one");
    let origin = *STATE_LOCK_ORIGIN;
    let mut first = observer.acquired(1, std::panic::Location::caller(), origin, origin);
    first.released = Some(1);
    let second = observer.acquired(
        1,
        std::panic::Location::caller(),
        origin,
        origin + Duration::from_micros(2),
    );
    let ring = observer.ring.lock().unwrap();
    observer.released(first);
    assert_eq!(observer.active().unwrap().unwrap().id, second.id);
    assert_eq!(observer.loss.load(SeqCst), 1);
    drop(ring);
    let mut second = second;
    second.released = Some(3);
    observer.released(second);
    assert!(observer.active().unwrap().is_none());
}

#[test]
fn active_evidence_is_labelled_by_age_and_only_overlapping_epoch_is_selected() {
    let observer = StateLockObserver::new("instance-one");
    let origin = *STATE_LOCK_ORIGIN;
    let active = observer.acquired(7, std::panic::Location::caller(), origin, origin);
    let block = StateLockBlock::collect_at(
        &observer,
        &waits(span(active.id + 1, 7, 0, 1, None)),
        0,
        100,
    );
    assert_eq!(block.holders.len(), 1);
    assert_eq!(block.holders[0].state, "active");
    assert_eq!(block.holders[0].age_micros, Some(100));
    assert!(block.holders[0].released_micros.is_none());
    let cross_epoch = StateLockBlock::collect_at(
        &observer,
        &waits(span(active.id + 1, 8, 0, 1, None)),
        0,
        100,
    );
    assert!(cross_epoch.holders.is_empty());
    assert!(cross_epoch.partial && cross_epoch.truncation_count == 1);
}

#[test]
fn fixed_ring_eviction_and_busy_or_poisoned_reads_are_explicitly_partial() {
    let observer = Arc::new(StateLockObserver::new("instance-one"));
    for id in 1..=260 {
        observer.released(span(id, 1, 0, 10, Some(30)));
    }
    {
        let ring = observer.ring.lock().unwrap();
        assert_eq!(ring.slots.len(), 256);
        assert_eq!(ring.count, 256);
        let block = StateLockBlock::collect(&observer, &waits(span(1000, 1, 20, 40, None)), 0);
        assert!(block.unavailable && block.partial);
        assert!(block.loss_count > 0);
    }
    let block = StateLockBlock::collect(&observer, &waits(span(1000, 1, 20, 40, None)), 0);
    assert_eq!(block.eviction_count, 4);
    assert_eq!(block.holders.len(), 4);
    assert!(block.partial && block.truncation_count > 0);
    let poisoned = Arc::clone(&observer);
    assert!(
        std::thread::spawn(move || {
            let _ring = poisoned.ring.lock().unwrap();
            panic!("fixture diagnostic poison");
        })
        .join()
        .is_err()
    );
    let block = StateLockBlock::collect(&observer, &waits(span(1000, 1, 20, 40, None)), 0);
    assert!(block.unavailable && block.partial);
    assert!(block.holders.is_empty());
}

#[test]
fn busy_or_poisoned_observer_cannot_hold_the_production_mutex() {
    let state = Arc::new(StateMutex::new(()));
    let ring = state.lock_observer.ring.lock().unwrap();
    drop(state.lock().unwrap());
    let worker = Arc::clone(&state);
    assert!(
        std::thread::spawn(move || worker.try_lock_now().is_some())
            .join()
            .unwrap()
    );
    drop(ring);
    let observer = Arc::clone(&state.lock_observer);
    assert!(
        std::thread::spawn(move || {
            let _ring = observer.ring.lock().unwrap();
            panic!("fixture diagnostic poison");
        })
        .join()
        .is_err()
    );
    assert!(state.try_lock_now().is_some());
    assert!(
        !state.inner.is_poisoned(),
        "diagnostic poison is separate from StateInner poison"
    );
}

#[test]
fn direct_try_lock_and_replaced_or_restored_inner_keep_distinct_domains() {
    let state = StateMutex::new(StateInner::new());
    let first = state.lock().unwrap().lock_span;
    let second = state.try_lock_now().unwrap().lock_span;
    assert_ne!(first.id, second.id);
    assert_eq!(first.epoch, second.epoch);
    *state.lock().unwrap() = StateInner::new();
    let third = state.lock().unwrap().lock_span;
    assert_ne!(second.epoch, third.epoch);
    let saved = PersistedState::from_inner(&state.lock().unwrap());
    *state.lock().unwrap() = saved.into_inner().unwrap();
    let restored = state.lock().unwrap().lock_span;
    assert_ne!(third.epoch, restored.epoch);
    assert!(restored.id > third.id);
    let other = StateMutex::new(());
    let other_id = other.lock().unwrap().lock_span.id;
    assert!(
        other_id > restored.id,
        "hold ids order all typed mutexes in this process"
    );
}

#[test]
fn actual_state_poison_remains_poison_and_try_lock_still_recovers_as_before() {
    let state = Arc::new(StateMutex::new(()));
    let poisoned = Arc::clone(&state);
    assert!(
        std::thread::spawn(move || {
            let _guard = poisoned.lock().unwrap();
            panic!("fixture StateInner poison");
        })
        .join()
        .is_err()
    );
    assert!(state.lock().is_err());
    assert!(state.try_lock_now().is_some());
}

#[test]
fn serialization_drops_rows_with_explicit_truncation_to_stay_bounded() {
    let observer = StateLockObserver::new("instance-one");
    let mut samples = [None; 8];
    for (index, entry) in samples.iter_mut().enumerate() {
        *entry = Some(span(1000 + index as u64, 1, 20, 40, None));
    }
    for id in 1..=10 {
        observer.released(span(id, 1, 0, 10, Some(30)));
    }
    let block = StateLockBlock::collect(&observer, &samples, 0);
    assert!(serde_json::to_vec(&block).unwrap().len() <= 2048);
    assert!(block.waits.len() <= 8 && block.holders.len() <= 4);
    assert!(block.truncation_count > 0 && block.partial);

    // Max-length permitted sites force the byte loop independently of the
    // row-count caps. The exact pre-bound block is deliberately oversized.
    let site = StateLockSite {
        file: format!("src/{}.rs", "a".repeat(89)),
        line: u32::MAX,
        column: u32::MAX,
    };
    assert_eq!(site.file.len(), 96);
    let mut oversized = StateLockBlock::empty("instance-one");
    oversized.mutex_epoch = "1".to_owned();
    for id in 1..=8 {
        oversized.waits.push(StateLockWait {
            hold_id: id.to_string(),
            site: site.clone(),
            requested_micros: 9_007_199_254_740_000,
            acquired_micros: 9_007_199_254_740_991,
        });
    }
    for id in 9..=12 {
        oversized.holders.push(StateLockHolder {
            hold_id: id.to_string(),
            site: site.clone(),
            acquired_micros: 9_007_199_254_739_000,
            released_micros: Some(9_007_199_254_740_001),
            age_micros: None,
            state: "released",
        });
    }
    assert!(serde_json::to_vec(&oversized).unwrap().len() > 2048);
    oversized.bound();
    assert!(serde_json::to_vec(&oversized).unwrap().len() <= 2048);
    assert!(oversized.partial && oversized.truncation_count > 0);
    assert_eq!(
        oversized.truncation_count as usize,
        12 - oversized.waits.len() - oversized.holders.len()
    );
}

#[test]
fn instance_labels_and_epoch_changes_prevent_reusing_prior_registration_waits() {
    let state = test_app_state();
    assert_eq!(state.inner.lock_observer.instance, state.server_instance_id);
    let diagnostic = TestRunRegistrationDiagnostics::new(&state.server_instance_id);
    drop(diagnostic.lock(&state.inner));
    let prior_epoch = state.inner.lock().unwrap().state_lock_epoch;
    *state.inner.lock().unwrap() = StateInner::new();
    drop(diagnostic.lock(&state.inner));
    let block = diagnostic.state_lock_block();
    assert_ne!(block.mutex_epoch, prior_epoch.to_string());
    assert_eq!(block.waits.len(), 1, "only the new domain can be projected");
    assert!(block.partial && block.truncation_count >= 1);
}

#[test]
fn publication_gaps_are_unknown_instead_of_a_fabricated_idle_holder() {
    use std::sync::atomic::Ordering::SeqCst;
    let observer = StateLockObserver::new("instance-one");
    observer.active_id.store(u64::MAX, SeqCst);
    assert!(observer.active().is_err());
    observer.active_id.store(0, SeqCst);
    observer.changing.store(1, SeqCst);
    let block = StateLockBlock::collect(&observer, &waits(span(100, 1, 20, 60, None)), 0);
    assert!(block.partial && block.unavailable);
    assert!(block.loss_count > 0);
}
