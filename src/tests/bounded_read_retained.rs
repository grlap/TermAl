// Owns fixed-capacity owner retention tests; no OS processes or syscalls.
// Does not test cleanup outcomes, covered by bounded_read_cleanup_budget.
use super::*;

#[test]
fn bounded_read_cleanup_retained_owner_survives_stringifying_error() {
    let owner = Arc::new(());
    let mut ring = BoundedReadRetainedOwners::new();
    let detail = "pid=12 reason=leader_alive signal_errno=Some(1) probe_errno=None".to_owned();
    assert!(ring.insert(12, owner.clone(), detail.clone()).is_none());
    assert_eq!(Arc::strong_count(&owner), 2);
    assert_eq!(ring.entries[0].detail, detail);
    assert!(ring.entries[0].retained);
}

#[test]
fn bounded_read_cleanup_retained_overflow_drops_oldest_without_cleanup() {
    struct DropOnly(Arc<Mutex<Vec<u32>>>, u32);
    impl Drop for DropOnly {
        fn drop(&mut self) {
            self.0.lock().unwrap().push(self.1);
        }
    }
    let drops = Arc::new(Mutex::new(Vec::new()));
    let mut ring = BoundedReadRetainedOwners::new();
    for pid in 1..=32 {
        assert!(
            ring.insert(pid, Arc::new(DropOnly(drops.clone(), pid)), "unresolved".into())
                .is_none()
        );
    }
    let evicted = ring
        .insert(33, Arc::new(DropOnly(drops.clone(), 33)), "unresolved".into())
        .unwrap();
    assert_eq!(evicted.pid, 1);
    assert_eq!(evicted.sequence, 1);
    assert!(!evicted.retained);
    assert_eq!(ring.entries.len(), 32);
    assert_eq!(ring.entries.front().unwrap().pid, 2);
    drop(evicted);
    assert_eq!(*drops.lock().unwrap(), [1]);
}

#[test]
fn bounded_read_cleanup_retained_later_reap_removes_only_exact_pid() {
    let mut ring = BoundedReadRetainedOwners::new();
    ring.insert(11, Arc::new(()), "first".into());
    ring.insert(12, Arc::new(()), "second".into());
    ring.remove_reaped(11);
    assert_eq!(ring.entries.len(), 1);
    assert_eq!(ring.entries[0].pid, 12);
}

#[test]
fn bounded_read_cleanup_retained_repeat_does_not_grow_the_ring() {
    let mut ring = BoundedReadRetainedOwners::new();
    let owner = Arc::new(());
    ring.insert(11, owner.clone(), "first".into());
    ring.insert(11, owner, "second".into());
    assert_eq!(ring.entries.len(), 1);
    assert_eq!(ring.entries[0].sequence, 2);
    assert_eq!(ring.entries[0].detail, "second");
}

#[test]
fn bounded_read_cleanup_retained_reused_pid_preserves_distinct_owners_in_both_orders() {
    for old_arrives_last in [false, true] {
        let old = Arc::new(());
        let newer = Arc::new(());
        let mut ring = BoundedReadRetainedOwners::new();
        let (first, second) = if old_arrives_last {
            (&newer, &old)
        } else {
            (&old, &newer)
        };
        assert!(ring.insert(41, first.clone(), "first".into()).is_none());
        assert!(ring.insert(41, second.clone(), "second".into()).is_none());
        assert_eq!(ring.entries.len(), 2, "distinct PID incarnations must survive");
        assert_eq!(Arc::strong_count(&old), 2);
        assert_eq!(Arc::strong_count(&newer), 2);

        ring.remove_reaped_matching(41, |owner| Arc::ptr_eq(owner, &old));
        assert_eq!(ring.entries.len(), 1);
        assert!(Arc::ptr_eq(&ring.entries[0]._owner, &newer));
        assert_eq!(Arc::strong_count(&old), 1);
    }
}

#[test]
fn bounded_read_cleanup_retained_refresh_matches_owner_not_just_pid() {
    let old = Arc::new(());
    let newer = Arc::new(());
    let mut ring = BoundedReadRetainedOwners::new();
    ring.insert(41, old.clone(), "old".into());
    ring.insert(41, newer.clone(), "newer".into());
    assert!(ring.insert(41, old.clone(), "old refreshed".into()).is_none());
    assert_eq!(ring.entries.len(), 2);
    assert!(Arc::ptr_eq(&ring.entries[0]._owner, &newer));
    assert!(Arc::ptr_eq(&ring.entries[1]._owner, &old));
    assert_eq!(ring.entries[1].sequence, 3);
    assert_eq!(ring.entries[1].detail, "old refreshed");
}

#[test]
fn bounded_read_cleanup_retained_pid_collision_at_capacity_reports_eviction() {
    let mut ring = BoundedReadRetainedOwners::new();
    let oldest = Arc::new(());
    ring.insert(41, oldest.clone(), "oldest".into());
    for pid in 42..73 {
        ring.insert(pid, Arc::new(()), "other".into());
    }
    let newer = Arc::new(());
    let evicted = ring.insert(41, newer.clone(), "new incarnation".into()).unwrap();
    assert_eq!(ring.entries.len(), 32);
    assert!(!evicted.retained);
    assert!(Arc::ptr_eq(&evicted._owner, &oldest));
    assert!(Arc::ptr_eq(&ring.entries.back().unwrap()._owner, &newer));
}
