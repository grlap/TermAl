// Owns immutable control timing and test phase notifications per channel.
// Does not own global Tokio time or persistence. Split from federation_runtime_io.rs.

use super::*;

#[derive(Clone)]
pub(crate) struct FederationControlSettings {
    pub(crate) interval: Duration,
    pub(crate) timeout: Duration,
    pub(crate) link_startup_timeout: Duration,
    pub(crate) attest_timeout: Duration,
    pub(crate) clock: FederationControlClock,
    #[cfg(test)]
    pub(crate) observed: Arc<dyn Fn(&'static str, u64) + Send + Sync>,
    /// The bounded expiry lines this channel's listener wrote, so a test can
    /// check what an expired request named.
    #[cfg(test)]
    pub(crate) deadline_expiries: Arc<Mutex<Vec<FederationDeadlineExpiry>>>,
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
            #[cfg(test)]
            deadline_expiries: Arc::default(),
        }
    }
}

impl FederationControlSettings {
    pub(crate) fn from_io(io: &FederationRuntimeIo) -> Self {
        Self {
            interval: io.control_interval,
            timeout: io.control_timeout,
            link_startup_timeout: io.link_startup_timeout,
            attest_timeout: io.attest_timeout,
            clock: io.control_clock.clone(),
            #[cfg(test)]
            observed: io.control_observed.clone(),
            #[cfg(test)]
            deadline_expiries: Arc::default(),
        }
    }

    pub(crate) fn observe(&self, event: &'static str, sequence: u64) {
        #[cfg(test)]
        (self.observed)(event, sequence);
        #[cfg(not(test))]
        let _ = (event, sequence);
    }
}

// Observation only: publish the terminal phase before the owned security guard
// withdraws this context. Keep the fixture's sequence notification as well.
pub(crate) struct FederationControlEnd<'a> {
    pub(crate) channel: &'a FederationChannel,
    pub(crate) context: &'a FederationSecurityContext,
    pub(crate) reason: &'static str,
    pub(crate) io_kind: Option<&'static str>,
    pub(crate) sequence: u64,
}

impl Drop for FederationControlEnd<'_> {
    fn drop(&mut self) {
        // Like FederationWorkerReporter, diagnostics must not hide the first
        // failure. The owning security guard still performs exact revocation.
        if std::thread::panicking() {
            return;
        }
        self.channel
            .observe_terminal_detail(self.context, "control", self.reason, self.io_kind);
        self.channel.control.observe(self.reason, self.sequence);
    }
}

impl FederationControlEnd<'_> {
    pub(crate) fn frame_failure(&mut self, failure: FederationControlFrameFailure) -> ApiError {
        self.reason = failure.reason;
        self.io_kind = failure.io_kind;
        failure.error
    }
}
