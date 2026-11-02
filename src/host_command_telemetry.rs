//! Test-only attribution of the existing process-wide inheritance mutex.
//! This module is mounted only with cfg(all(windows, test)).
#![cfg(all(windows, test))]
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

const HISTORY_CAPACITY: usize = 4096;

#[derive(Clone, Debug, serde::Serialize)]
struct Phase {
    name: &'static str,
    started_ns: u128,
    ended_ns: u128,
}

#[derive(Clone, Debug, serde::Serialize)]
struct Hold {
    site: &'static str,
    file: &'static str,
    line: u32,
    pid: u32,
    thread_id: String,
    thread_name: Option<String>,
    // Libtest names its test worker after the test. Auxiliary threads can be
    // unnamed or have another name; their test identity stays unknown.
    test_thread_name: Option<String>,
    acquired_ns: u128,
    released_ns: u128,
    hold_ns: u128,
    phases: Vec<Phase>,
}

#[derive(Default)]
struct History {
    holds: VecDeque<Hold>,
    evicted_through_ns: Option<u128>,
}

impl History {
    fn push(&mut self, hold: Hold, capacity: usize) {
        self.holds.push_back(hold);
        while self.holds.len() > capacity {
            if let Some(evicted) = self.holds.pop_front() {
                self.evicted_through_ns = Some(evicted.released_ns);
            }
        }
    }

    fn intersecting(&self, started_ns: u128, ended_ns: u128) -> WaitSummary {
        let mut result = WaitSummary {
            waits: 1,
            first_wait_started_ns: Some(started_ns),
            last_wait_ended_ns: Some(ended_ns),
            total_wait_ns: ended_ns.saturating_sub(started_ns),
            history_overflow: self.evicted_through_ns.is_some_and(|end| end > started_ns),
            ..WaitSummary::default()
        };
        for hold in &self.holds {
            if hold.acquired_ns < ended_ns && hold.released_ns > started_ns {
                result.retained_count += 1;
                if result
                    .retained_max
                    .as_ref()
                    .is_none_or(|max| hold.hold_ns > max.hold_ns)
                {
                    result.retained_max = Some(hold.clone());
                }
            }
        }
        result
    }
}

#[derive(Clone, Debug, Default, serde::Serialize)]
struct WaitSummary {
    waits: usize,
    first_wait_started_ns: Option<u128>,
    last_wait_ended_ns: Option<u128>,
    total_wait_ns: u128,
    history_overflow: bool,
    retained_count: usize,
    retained_max: Option<Hold>,
}

impl WaitSummary {
    fn merge(&mut self, other: Self) {
        self.waits += other.waits;
        self.total_wait_ns += other.total_wait_ns;
        if let Some(start) = other.first_wait_started_ns {
            self.first_wait_started_ns = Some(
                self.first_wait_started_ns
                    .map_or(start, |old| old.min(start)),
            );
        }
        if let Some(end) = other.last_wait_ended_ns {
            self.last_wait_ended_ns = Some(self.last_wait_ended_ns.map_or(end, |old| old.max(end)));
        }
        self.history_overflow |= other.history_overflow;
        self.retained_count += other.retained_count;
        if let Some(max) = other.retained_max {
            if self
                .retained_max
                .as_ref()
                .is_none_or(|previous| max.hold_ns > previous.hold_ns)
            {
                self.retained_max = Some(max);
            }
        }
    }
}

static ORIGIN: OnceLock<Instant> = OnceLock::new();
static HISTORY: OnceLock<Mutex<History>> = OnceLock::new();
thread_local! {
    static OBSERVATION: RefCell<Option<Rc<RefCell<WaitSummary>>>> = const { RefCell::new(None) };
}

fn now_ns() -> u128 {
    ORIGIN.get_or_init(Instant::now).elapsed().as_nanos()
}

fn history() -> MutexGuard<'static, History> {
    HISTORY
        .get_or_init(|| Mutex::new(History::default()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

/// A bounded read observes only acquisitions on its own thread and restores
/// an enclosing observation on drop. Other threads still contribute holds.
pub(crate) struct Observation {
    summary: Rc<RefCell<WaitSummary>>,
    previous: Option<Rc<RefCell<WaitSummary>>>,
}

impl Observation {
    pub(crate) fn new() -> Self {
        let summary = Rc::new(RefCell::new(WaitSummary::default()));
        let previous = OBSERVATION.with(|slot| slot.replace(Some(summary.clone())));
        Self { summary, previous }
    }

    pub(crate) fn summary(&self) -> String {
        let summary = self.summary.borrow();
        let certainty = if summary.history_overflow {
            "unknown"
        } else {
            "complete"
        };
        // Retained values are explicitly partial when the history overflowed.
        format!(
            "inheritance_holders={certainty} {}",
            serde_json::to_string(&*summary)
                .unwrap_or_else(|_| "serialization unavailable".to_owned())
        )
    }
}

impl Drop for Observation {
    fn drop(&mut self) {
        OBSERVATION.with(|slot| {
            slot.replace(self.previous.take());
        });
    }
}

pub(crate) struct Guard {
    guard: Option<MutexGuard<'static, ()>>,
    hold: Hold,
    phase: &'static str,
    phase_started_ns: u128,
}

#[track_caller]
pub(crate) fn acquire(site: &'static str) -> Guard {
    let caller = std::panic::Location::caller();
    let thread = std::thread::current();
    let thread_id = format!("{:?}", thread.id());
    let thread_name = thread.name().map(str::to_owned);
    let test_thread_name = thread_name
        .as_ref()
        .filter(|name| name.contains("::"))
        .cloned();
    let phases = Vec::with_capacity(8);
    let observation = OBSERVATION.with(|slot| slot.borrow().clone());
    let started_ns = now_ns();
    let guard = super::inheritance_lock();
    let acquired_ns = now_ns();
    if let Some(observation) = observation {
        observation
            .borrow_mut()
            .merge(history().intersecting(started_ns, acquired_ns));
    }
    Guard {
        guard: Some(guard),
        hold: Hold {
            site,
            file: caller.file(),
            line: caller.line(),
            pid: std::process::id(),
            thread_id,
            thread_name,
            test_thread_name,
            acquired_ns,
            released_ns: acquired_ns,
            hold_ns: 0,
            phases,
        },
        phase: "spawn",
        phase_started_ns: acquired_ns,
    }
}

impl Guard {
    pub(crate) fn phase(&mut self, name: &'static str) {
        let now = now_ns();
        self.hold.phases.push(Phase {
            name: self.phase,
            started_ns: self.phase_started_ns,
            ended_ns: now,
        });
        self.phase = name;
        self.phase_started_ns = now;
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // Lock order is inheritance -> history. No path holds history while
        // waiting for inheritance. Keep history locked across unlock+record:
        // the next acquirer cannot miss the just-released overlapping owner.
        let mut history = history();
        // Sample immediately BEFORE unlock: a descheduled thread after
        // unlocking must not report its off-lock delay as lock hold time.
        let released_ns = now_ns();
        drop(self.guard.take());
        self.hold.released_ns = released_ns;
        self.hold.hold_ns = released_ns.saturating_sub(self.hold.acquired_ns);
        self.hold.phases.push(Phase {
            name: self.phase,
            started_ns: self.phase_started_ns,
            ended_ns: released_ns,
        });
        history.push(self.hold.clone(), HISTORY_CAPACITY);
        drop(history);
        // History is consumed only by a probe's failure summary. Successful
        // fixture launches must not stream thousands of diagnostic lines.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hold(site: &'static str, start: u128, end: u128) -> Hold {
        Hold {
            site,
            file: "fixture",
            line: 1,
            pid: 1,
            thread_id: "fixture".to_owned(),
            thread_name: None,
            test_thread_name: None,
            acquired_ns: start,
            released_ns: end,
            hold_ns: end - start,
            phases: vec![],
        }
    }

    #[test]
    fn interval_includes_owner_already_active_at_wait_start_and_excludes_touching_edges() {
        let mut history = History::default();
        for entry in [
            hold("before", 0, 10),
            hold("active", 5, 25),
            hold("short", 26, 30),
            hold("after", 40, 50),
        ] {
            history.push(entry, 8);
        }
        let summary = history.intersecting(10, 40);
        assert!(!summary.history_overflow);
        assert_eq!(summary.retained_count, 2);
        let max = summary.retained_max.unwrap();
        assert_eq!(max.site, "active");
        assert_eq!(max.hold_ns, 20);
    }

    #[test]
    fn evicted_intersecting_hold_is_unknown_but_old_eviction_is_not() {
        let mut history = History::default();
        history.push(hold("evicted", 0, 20), 1);
        history.push(hold("retained", 21, 30), 1);
        let incomplete = history.intersecting(10, 40);
        assert!(incomplete.history_overflow);
        assert_eq!(incomplete.retained_count, 1);
        assert!(!history.intersecting(20, 40).history_overflow);
    }

    #[test]
    fn convoy_summary_uses_interval_max_instead_of_lifetime_high_water() {
        let mut history = History::default();
        history.push(hold("old_long", 0, 1000), 8);
        history.push(hold("first_short", 1001, 1010), 8);
        history.push(hold("second_short", 1011, 1030), 8);
        let summary = history.intersecting(1000, 1040);
        assert_eq!(summary.retained_count, 2);
        assert_eq!(summary.retained_max.as_ref().unwrap().site, "second_short");
        assert_eq!(summary.retained_max.as_ref().unwrap().hold_ns, 19);
        assert!(!summary.history_overflow);
    }

    #[test]
    fn multiple_launch_attempts_keep_maximum_and_unknown_status() {
        let mut summary = WaitSummary {
            waits: 1,
            retained_count: 2,
            retained_max: Some(hold("long", 0, 20)),
            history_overflow: false,
            ..WaitSummary::default()
        };
        summary.merge(WaitSummary {
            waits: 1,
            retained_count: 1,
            retained_max: Some(hold("short", 30, 35)),
            history_overflow: true,
            ..WaitSummary::default()
        });
        assert_eq!(summary.waits, 2);
        assert_eq!(summary.retained_count, 3);
        assert_eq!(summary.retained_max.as_ref().unwrap().site, "long");
        assert!(summary.history_overflow);
        let observation = Observation::new();
        *observation.summary.borrow_mut() = summary;
        assert!(
            observation
                .summary()
                .starts_with("inheritance_holders=unknown ")
        );
    }
}
