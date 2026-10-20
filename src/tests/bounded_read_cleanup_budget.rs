// Owns deterministic cleanup-budget and unresolved-notification regressions.
// Does not sleep or launch processes. Extends the bounded_read_cleanup seam tests.
use super::*;

struct BudgetFixture {
    observations: VecDeque<bool>,
    last_observation: bool,
    remaining_ticks: usize,
    signal_errno: Option<i32>,
    trace: Vec<&'static str>,
    retained: Vec<String>,
}

impl BudgetFixture {
    fn new(observations: &[bool], remaining_ticks: usize, signal_errno: Option<i32>) -> Self {
        Self {
            observations: observations.iter().copied().collect(),
            last_observation: false,
            remaining_ticks,
            signal_errno,
            trace: Vec::new(),
            retained: Vec::new(),
        }
    }
}

impl BoundedReadGroupIo for BudgetFixture {
    type Status = u32;
    fn signal_before_reap(&mut self) -> io::Result<()> {
        assert!(!self.trace.contains(&"reap"));
        self.trace.push("signal");
        self.signal_errno
            .map_or(Ok(()), |n| Err(io::Error::from_raw_os_error(n)))
    }
    fn observe_before_reap(&mut self) -> io::Result<bool> {
        assert!(!self.trace.contains(&"reap"));
        self.trace.push("observe");
        if let Some(value) = self.observations.pop_front() {
            self.last_observation = value;
        }
        Ok(self.last_observation)
    }
    fn reap(&mut self) -> io::Result<Self::Status> {
        self.trace.push("reap");
        Ok(23)
    }
    fn probe_after_reap(&mut self) -> io::Result<()> {
        assert!(self.trace.contains(&"reap"));
        self.trace.push("probe");
        Err(io::Error::from_raw_os_error(3))
    }
    fn retry_probe(&mut self) -> bool {
        self.trace.push("tick");
        if self.remaining_ticks == 0 {
            return false;
        }
        self.remaining_ticks -= 1;
        true
    }
    fn missing_errno(&self) -> i32 {
        3
    }
    fn permission_errno(&self) -> i32 {
        1
    }
    fn process_id(&self) -> u32 {
        4243
    }
    fn retain_unresolved(&mut self, detail: String) {
        self.retained.push(detail);
    }
}

#[test]
fn bounded_read_cleanup_budget_live_leader_never_reaps_or_probes() {
    let mut ops = BudgetFixture::new(&[false], 2, Some(1));
    let error = finish_bounded_read_group(&mut ops).unwrap_err().to_string();
    assert!(error.contains("leader_alive"), "{error}");
    assert!(error.contains("pid=4243"), "{error}");
    assert!(error.contains("signal_errno=Some(1)"), "{error}");
    assert!(!ops.trace.contains(&"reap"));
    assert!(!ops.trace.contains(&"probe"));
    assert_eq!(ops.trace.iter().filter(|&&s| s == "observe").count(), 3);
    assert_eq!(ops.retained, [error]);
}

#[test]
fn bounded_read_cleanup_budget_exit_within_budget_reaps_then_confirms() {
    let mut ops = BudgetFixture::new(&[false, true], 1, Some(1));
    assert_eq!(finish_bounded_read_group(&mut ops).unwrap(), 23);
    assert_eq!(
        ops.trace,
        ["signal", "observe", "tick", "observe", "reap", "probe"]
    );
    assert!(ops.retained.is_empty());
}

#[test]
fn bounded_read_cleanup_budget_zero_remaining_still_probes_once_after_reap() {
    let mut ops = BudgetFixture::new(&[true], 0, None);
    assert_eq!(finish_bounded_read_group(&mut ops).unwrap(), 23);
    assert_eq!(ops.trace, ["signal", "observe", "reap", "probe"]);
    assert!(ops.retained.is_empty());
}

#[test]
fn bounded_read_cleanup_budget_reap_failure_retains_diagnostic_owner() {
    struct FailedReap(BudgetFixture);
    impl BoundedReadGroupIo for FailedReap {
        type Status = u32;
        fn signal_before_reap(&mut self) -> io::Result<()> {
            self.0.signal_before_reap()
        }
        fn observe_before_reap(&mut self) -> io::Result<bool> {
            self.0.observe_before_reap()
        }
        fn reap(&mut self) -> io::Result<u32> {
            Err(io::Error::from_raw_os_error(10))
        }
        fn probe_after_reap(&mut self) -> io::Result<()> {
            panic!("probe after failed reap")
        }
        fn retry_probe(&mut self) -> bool {
            self.0.retry_probe()
        }
        fn missing_errno(&self) -> i32 {
            3
        }
        fn permission_errno(&self) -> i32 {
            1
        }
        fn process_id(&self) -> u32 {
            4243
        }
        fn retain_unresolved(&mut self, detail: String) {
            self.0.retain_unresolved(detail);
        }
    }
    let mut ops = FailedReap(BudgetFixture::new(&[true], 0, None));
    let error = finish_bounded_read_group(&mut ops).unwrap_err().to_string();
    assert!(error.contains("reap"), "{error}");
    assert!(error.contains("pid=4243"), "{error}");
    assert_eq!(ops.0.retained, [error]);
}
