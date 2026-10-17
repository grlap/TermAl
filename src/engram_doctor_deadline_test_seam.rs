// Test-only deadline arming for the real doctor process-tree fixtures.
//
// Owns an RAII registration per fixture home: startup uses the test's existing
// deadlock guard, and the measured diagnostic budget starts when the parent
// owns the ready descendant and its process probes. Dropping the registration
// cancels a worker still waiting for that acknowledgement.
//
// Does not change production deadlines, unregistered diagnostic calls, process
// tree cleanup, or the staged-readiness/expired-launch seams. Included from
// main.rs only under cfg(test).

struct TestEngramDoctorDeadlineState {
    startup_deadline: std::time::Instant,
    receiver: Mutex<Option<mpsc::Receiver<std::time::Instant>>>,
}

static TEST_ENGRAM_DOCTOR_DEADLINE_HOMES: LazyLock<
    Mutex<HashMap<PathBuf, Arc<TestEngramDoctorDeadlineState>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

#[must_use = "retain the registration until the fixture worker has finished"]
struct TestEngramDoctorDeadlineArm {
    home: PathBuf,
    state: Arc<TestEngramDoctorDeadlineState>,
    sender: Option<mpsc::Sender<std::time::Instant>>,
}

impl TestEngramDoctorDeadlineArm {
    fn arm(&mut self) -> std::result::Result<(), String> {
        self.sender
            .take()
            .ok_or_else(|| "doctor fixture deadline was already armed".to_owned())?
            .send(std::time::Instant::now())
            .map_err(|_| "doctor worker stopped before its deadline was armed".to_owned())
    }
}

impl Drop for TestEngramDoctorDeadlineArm {
    fn drop(&mut self) {
        // Disconnect before removing the registration, so an in-flight worker
        // holding its cloned state is released too. Never wait under the map.
        drop(self.sender.take());
        let mut homes = TEST_ENGRAM_DOCTOR_DEADLINE_HOMES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if homes
            .get(&self.home)
            .is_some_and(|state| Arc::ptr_eq(state, &self.state))
        {
            homes.remove(&self.home);
        }
    }
}

fn register_test_engram_doctor_deadline(
    home: &FsPath,
    startup_guard: Duration,
) -> TestEngramDoctorDeadlineArm {
    let home = test_engram_readiness_seam_key(home);
    let (sender, receiver) = mpsc::channel();
    let state = Arc::new(TestEngramDoctorDeadlineState {
        startup_deadline: std::time::Instant::now() + startup_guard,
        receiver: Mutex::new(Some(receiver)),
    });
    let duplicate = {
        let mut homes = TEST_ENGRAM_DOCTOR_DEADLINE_HOMES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if homes.contains_key(&home) {
            true
        } else {
            homes.insert(home.clone(), Arc::clone(&state));
            false
        }
    };
    assert!(!duplicate, "doctor fixture home is already registered");
    TestEngramDoctorDeadlineArm {
        home,
        state,
        sender: Some(sender),
    }
}

fn test_engram_doctor_deadline_state(
    home: &FsPath,
    diagnostic: &str,
) -> Option<Arc<TestEngramDoctorDeadlineState>> {
    if diagnostic != "doctor" {
        return None;
    }
    let home = test_engram_readiness_seam_key(home);
    TEST_ENGRAM_DOCTOR_DEADLINE_HOMES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&home)
        .cloned()
}

fn test_engram_doctor_fixture_startup_deadline(
    state: Option<&TestEngramDoctorDeadlineState>,
    original: std::time::Instant,
) -> std::time::Instant {
    state.map_or(original, |state| state.startup_deadline)
}

fn test_engram_doctor_fixture_armed_deadline(
    state: Option<&TestEngramDoctorDeadlineState>,
    original: std::time::Instant,
    budget: Duration,
) -> std::result::Result<std::time::Instant, ApiError> {
    let Some(state) = state else {
        return Ok(original);
    };
    let receiver = state
        .receiver
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
        .ok_or_else(|| ApiError::bad_request("doctor fixture deadline was consumed twice"))?;
    let remaining = state
        .startup_deadline
        .saturating_duration_since(std::time::Instant::now());
    let armed_at = receiver.recv_timeout(remaining).map_err(|error| {
        ApiError::bad_request(format!(
            "doctor fixture readiness did not arm its deadline: {error}"
        ))
    })?;
    if armed_at >= state.startup_deadline {
        return Err(ApiError::bad_request(
            "doctor fixture startup exceeded its deadlock guard",
        ));
    }
    Ok(armed_at + budget)
}
