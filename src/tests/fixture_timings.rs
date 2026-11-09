//! Optional, bounded diagnostics for fixture work. No timing decides a test.
use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::time::Instant;

thread_local! {
    static ACTIVE_TIMING: RefCell<Option<Arc<Mutex<Timing>>>> = const { RefCell::new(None) };
}

#[derive(Default, Serialize)]
struct Phase {
    entries: u64,
    elapsed_us: u64,
    main_acquisitions: u64,
    main_wait_us: u64,
    main_hold_us: u64,
    background_acquisitions: u64,
    background_wait_us: u64,
    background_hold_us: u64,
}

struct Timing {
    name: &'static str,
    thread: std::thread::ThreadId,
    started: Instant,
    changed: Instant,
    current: &'static str,
    phases: BTreeMap<&'static str, Phase>,
}

impl Timing {
    fn change(&mut self, phase: &'static str) -> &'static str {
        let now = Instant::now();
        self.phases.entry(self.current).or_default().elapsed_us +=
            now.duration_since(self.changed).as_micros() as u64;
        let previous = self.current;
        self.current = phase;
        self.changed = now;
        self.phases.entry(phase).or_default().entries += 1;
        previous
    }
}

/// Declare before the fixture, so automatic fixture drops are included in the
/// final scope. Async workers that outlive that scope are not cleanup samples.
pub(super) struct FixtureTimings {
    data: Arc<Mutex<Timing>>,
    previous: Option<Arc<Mutex<Timing>>>,
}

impl FixtureTimings {
    pub(super) fn start(name: &'static str) -> Self {
        let now = Instant::now();
        let data = Arc::new(Mutex::new(Timing {
            name,
            thread: std::thread::current().id(),
            started: now,
            changed: now,
            current: "test_body",
            phases: BTreeMap::new(),
        }));
        let previous = ACTIVE_TIMING.with(|slot| slot.replace(Some(data.clone())));
        Self { data, previous }
    }

    pub(super) fn phase(&self, phase: &'static str) {
        self.data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .change(phase);
    }
}

impl Drop for FixtureTimings {
    fn drop(&mut self) {
        ACTIVE_TIMING.with(|slot| slot.replace(self.previous.take()));
        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        data.change("report_complete");
        let report = json!({
            "test":data.name, "elapsed_us":data.started.elapsed().as_micros(),
            "phases":data.phases,
            "scope":"exclusive test-thread elapsed phases; synchronous drops only",
            "waits":"StateMutex only; background sums overlap; plain mutex and async cleanup attribution unavailable",
            "backgroundPhase":"test-thread phase at lock release, not a causal wait attribution",
        });
        let _ = std::io::Write::write_all(
            &mut std::io::stderr(),
            format!("FIXTURE_PHASE_TIMING {report}\n").as_bytes(),
        );
    }
}

pub(super) struct FixturePhase {
    previous: Option<(Arc<Mutex<Timing>>, &'static str)>,
}

pub(super) fn fixture_phase(phase: &'static str) -> FixturePhase {
    let previous = ACTIVE_TIMING
        .with(|slot| slot.borrow().clone())
        .map(|data| {
            let previous = data.lock().unwrap_or_else(|e| e.into_inner()).change(phase);
            (data, previous)
        });
    FixturePhase { previous }
}

impl Drop for FixturePhase {
    fn drop(&mut self) {
        if let Some((data, phase)) = &self.previous {
            data.lock().unwrap_or_else(|e| e.into_inner()).change(phase);
        }
    }
}

/// Install the existing test-only lock reporter before the state is shared.
/// Inactive tests retain their original mutex and instance identity unchanged.
pub(super) fn timed_fixture_state_mutex(
    value: StateInner,
    instance: &str,
) -> StateMutex<StateInner> {
    let mut mutex = StateMutex::new_for_instance(value, instance);
    if let Some(data) = ACTIVE_TIMING.with(|slot| slot.borrow().clone()) {
        mutex.warn_after = Duration::ZERO;
        mutex.diagnostic_reporter = Arc::new(move |event| {
            // Held already carries acquisition wait; counting Waited as well
            // would count that same wait twice.
            if let StateMutexDiagnostic::Held { waited, held, .. } = event {
                let mut data = data.lock().unwrap_or_else(|e| e.into_inner());
                let main = std::thread::current().id() == data.thread;
                let phase = data.current;
                let row = data.phases.entry(phase).or_default();
                if main {
                    row.main_acquisitions += 1;
                    row.main_wait_us += waited.as_micros() as u64;
                    row.main_hold_us += held.as_micros() as u64;
                } else {
                    row.background_acquisitions += 1;
                    row.background_wait_us += waited.as_micros() as u64;
                    row.background_hold_us += held.as_micros() as u64;
                }
            }
        });
    }
    mutex
}
