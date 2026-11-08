//! Scheduling for the process-heavy identity CLI integration suite only.
//! Every fixture, including nested and capacity cases, owns one mutex through
//! cleanup. The named guard must be the first local in each test and must never
//! be acquired in Fixture::new or twice in one test: this lock is not reentrant.
//! fixture_ms excludes lock waiting and includes setup, assertions and cleanup;
//! wait_ms reports the lock wait separately. Both are diagnostic only.
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

static FIXTURE_GROUP: FixtureTestGroup = FixtureTestGroup::new();

struct FixtureTestGroup {
    gate: Mutex<()>,
}

impl FixtureTestGroup {
    const fn new() -> Self {
        Self {
            gate: Mutex::new(()),
        }
    }

    fn exclusive(&self) -> MutexGuard<'_, ()> {
        self.gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub struct FixtureGuard {
    _guard: MutexGuard<'static, ()>,
    started: Instant,
    waited: Duration,
}

impl Drop for FixtureGuard {
    fn drop(&mut self) {
        let current = std::thread::current();
        eprintln!(
            "identity fixture elapsed: test={} fixture_ms={}",
            current.name().unwrap_or("unnamed"),
            self.started.elapsed().as_millis()
        );
        eprintln!(
            "identity fixture wait: test={} wait_ms={}",
            current.name().unwrap_or("unnamed"),
            self.waited.as_millis()
        );
    }
}

pub fn exclusive() -> FixtureGuard {
    let requested = Instant::now();
    let guard = FIXTURE_GROUP.exclusive();
    let started = Instant::now();
    FixtureGuard {
        _guard: guard,
        started,
        waited: started.duration_since(requested),
    }
}

#[test]
fn exclusive_fixture_test_excludes_other_fixture_work_through_release() {
    let group = FixtureTestGroup::new();
    let exclusive = group.exclusive();
    assert!(
        matches!(
            group.gate.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        ),
        "a fixture must exclude concurrent fixture work"
    );
    drop(exclusive);
    assert!(
        group.gate.try_lock().is_ok(),
        "release admits the next fixture"
    );
}
