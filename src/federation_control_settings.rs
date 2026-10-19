// Owns immutable control timing and test phase notifications per channel.
// Does not own global Tokio time or persistence. Split from federation_runtime_io.rs.

#[derive(Clone)]
struct FederationControlSettings {
    interval: Duration,
    timeout: Duration,
    link_startup_timeout: Duration,
    attest_timeout: Duration,
    clock: FederationControlClock,
    #[cfg(test)]
    observed: Arc<dyn Fn(&'static str, u64) + Send + Sync>,
}

impl Default for FederationControlSettings {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(15),
            timeout: Duration::from_secs(45),
            link_startup_timeout: FEDERATION_LINK_STARTUP_TIMEOUT,
            attest_timeout: FEDERATION_ATTEST_TIMEOUT,
            clock: FederationControlClock::default(),
            #[cfg(test)]
            observed: Arc::new(|_, _| {}),
        }
    }
}

impl FederationControlSettings {
    fn from_io(io: &FederationRuntimeIo) -> Self {
        Self {
            interval: io.control_interval,
            timeout: io.control_timeout,
            link_startup_timeout: io.link_startup_timeout,
            attest_timeout: io.attest_timeout,
            clock: io.control_clock.clone(),
            #[cfg(test)]
            observed: io.control_observed.clone(),
        }
    }

    fn observe(&self, event: &'static str, sequence: u64) {
        #[cfg(test)]
        (self.observed)(event, sequence);
        #[cfg(not(test))]
        let _ = (event, sequence);
    }
}

// Observation only: every central-control return has a terminal phase. A
// fixture must distinguish the specified deadline from an earlier IO failure.
struct FederationControlEnd<'a> {
    settings: &'a FederationControlSettings,
    reason: &'static str,
    sequence: u64,
}

impl Drop for FederationControlEnd<'_> {
    fn drop(&mut self) {
        self.settings.observe(self.reason, self.sequence);
    }
}
