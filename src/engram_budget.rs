// Monotonic time for Engram authority/admission operations. Production uses
// real time; positive fixtures explicitly install one shared scripted clock.
// SQLite, transport results and ownership checks remain independent of time.
#[derive(Clone, Default)]
enum EngramBudgetClock {
    #[default]
    Real,
    #[cfg(test)]
    Scripted(Arc<ScriptedEngramBudgetClock>),
}

#[cfg(test)]
struct ScriptedEngramBudgetClock {
    now: Mutex<std::time::Instant>,
    event: Mutex<u64>,
    changed: Condvar,
    waiters: std::sync::atomic::AtomicUsize,
}

impl EngramBudgetClock {
    fn now(&self) -> std::time::Instant {
        match self {
            Self::Real => std::time::Instant::now(),
            #[cfg(test)]
            Self::Scripted(clock) => *clock.now.lock().expect("budget clock mutex poisoned"),
        }
    }

    fn elapsed_since(&self, started: std::time::Instant) -> Duration {
        self.now().saturating_duration_since(started)
    }

    fn notify(&self) {
        #[cfg(test)]
        if let Self::Scripted(clock) = self {
            let mut event = clock.event.lock().expect("budget event mutex poisoned");
            *event = event.checked_add(1).expect("budget event exhausted");
            clock.changed.notify_all();
        }
    }

    #[cfg(test)]
    fn scripted() -> Self {
        Self::Scripted(Arc::new(ScriptedEngramBudgetClock {
            now: Mutex::new(std::time::Instant::now()),
            event: Mutex::new(0),
            changed: Condvar::new(),
            waiters: std::sync::atomic::AtomicUsize::new(0),
        }))
    }

    #[cfg(test)]
    fn advance(&self, duration: Duration) {
        let Self::Scripted(clock) = self else {
            panic!("only a scripted clock can be advanced");
        };
        {
            let mut now = clock.now.lock().expect("budget clock mutex poisoned");
            *now = now
                .checked_add(duration)
                .expect("scripted budget time exhausted");
        }
        self.notify();
    }

    /// Poll while holding only this clock's notification mutex. Polls must
    /// not block or notify the clock. Producers notify AFTER releasing their
    /// result mutex, so completion cannot invert the waiter lock order.
    #[cfg(test)]
    fn wait_scripted<T>(
        &self,
        until: std::time::Instant,
        mut poll: impl FnMut(std::time::Instant) -> Option<T>,
    ) -> Option<T> {
        let Self::Scripted(clock) = self else {
            panic!("scripted driver requires its own clock");
        };
        // This bounds a broken fixture driver, not its logical operation.
        // A watchdog is a test failure and can never acknowledge authority.
        let watchdog = std::time::Instant::now() + Duration::from_secs(30);
        let mut event = clock.event.lock().expect("budget event mutex poisoned");
        loop {
            let now = self.now();
            if let Some(result) = poll(now) {
                return Some(result);
            }
            if now >= until {
                return None;
            }
            let remaining = watchdog.saturating_duration_since(std::time::Instant::now());
            assert!(
                !remaining.is_zero(),
                "scripted Engram driver watchdog expired"
            );
            clock
                .waiters
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            clock.changed.notify_all();
            (event, _) = clock
                .changed
                .wait_timeout(event, remaining)
                .expect("budget event mutex poisoned");
            clock
                .waiters
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[cfg(test)]
    fn wait_for_scripted_waiter(&self) {
        let Self::Scripted(clock) = self else {
            panic!("scripted clock required")
        };
        let watchdog = std::time::Instant::now() + Duration::from_secs(30);
        let mut event = clock.event.lock().expect("budget event mutex poisoned");
        while clock.waiters.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            let remaining = watchdog.saturating_duration_since(std::time::Instant::now());
            assert!(
                !remaining.is_zero(),
                "scripted waiter never entered its driver"
            );
            (event, _) = clock
                .changed
                .wait_timeout(event, remaining)
                .expect("budget event mutex poisoned");
        }
    }

    fn recv_until<T>(
        &self,
        receiver: &mpsc::Receiver<T>,
        until: std::time::Instant,
    ) -> std::result::Result<T, mpsc::RecvTimeoutError> {
        match self {
            Self::Real => receiver.recv_timeout(until.saturating_duration_since(self.now())),
            #[cfg(test)]
            Self::Scripted(_) => self
                .wait_scripted(until, |_| match receiver.try_recv() {
                    Ok(value) => Some(Ok(value)),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        Some(Err(mpsc::RecvTimeoutError::Disconnected))
                    }
                    Err(mpsc::TryRecvError::Empty) => None,
                })
                .unwrap_or(Err(mpsc::RecvTimeoutError::Timeout)),
        }
    }
}

impl AppState {
    fn engram_budget_clock(&self) -> EngramBudgetClock {
        // Real time is the sole production clock. Acquiring the fixture's
        // dependency must not add a new production lock before budget start.
        #[cfg(not(test))]
        {
            EngramBudgetClock::Real
        }
        #[cfg(test)]
        {
            self.inner
                .lock()
                .expect("state mutex poisoned")
                .engram_budget_clock
                .clone()
        }
    }

    #[cfg(test)]
    fn install_test_engram_budget_clock(&self, clock: EngramBudgetClock) {
        self.inner
            .lock()
            .expect("state mutex poisoned")
            .engram_budget_clock = clock;
    }

    #[cfg(test)]
    fn test_engram_authority_ack_boundary(&self, boundary: &str) {
        let hook = self
            .inner
            .lock()
            .expect("state mutex poisoned")
            .test_engram_authority_ack_boundary
            .clone();
        if let Some(hook) = hook {
            hook(boundary);
        }
    }
}
