// Owns monotonic control deadlines and an explicit fixture clock.
// Does not change persistence, RPC limits or global Tokio time. New federation timing boundary.

#[derive(Clone)]
struct FederationControlClock {
    clock: EngramBudgetClock,
    #[cfg(test)]
    changed: Arc<tokio::sync::watch::Sender<u64>>,
    #[cfg(test)]
    io_deadlines: bool,
}

impl Default for FederationControlClock {
    fn default() -> Self {
        Self {
            clock: EngramBudgetClock::Real,
            #[cfg(test)]
            changed: Arc::new(tokio::sync::watch::channel(0).0),
            #[cfg(test)]
            io_deadlines: false,
        }
    }
}

impl FederationControlClock {
    // Production keeps Tokio's original deadlines. Only an explicitly opted-in
    // fixture drives protocol deadlines with the existing per-channel clock.
    async fn timeout<F: std::future::Future>(
        &self,
        duration: Duration,
        future: F,
    ) -> Result<F::Output, ()> {
        #[cfg(test)]
        if self.io_deadlines {
            return self.scripted_timeout(self.now() + duration, future).await;
        }
        tokio::time::timeout(duration, future).await.map_err(|_| ())
    }

    async fn timeout_at<F: std::future::Future>(
        &self,
        until: tokio::time::Instant,
        future: F,
    ) -> Result<F::Output, ()> {
        #[cfg(test)]
        if self.io_deadlines {
            return self.scripted_timeout(until.into_std(), future).await;
        }
        tokio::time::timeout_at(until, future).await.map_err(|_| ())
    }

    #[cfg(test)]
    async fn scripted_timeout<F: std::future::Future>(
        &self,
        until: std::time::Instant,
        future: F,
    ) -> Result<F::Output, ()> {
        tokio::select! {
            biased;
            result = future => Ok(result),
            () = self.deadline(until) => Err(()),
        }
    }

    fn now(&self) -> std::time::Instant {
        self.clock.now()
    }

    async fn deadline(&self, until: std::time::Instant) {
        #[cfg(test)]
        if matches!(self.clock, EngramBudgetClock::Scripted(_)) {
            let mut changed = self.changed.subscribe();
            loop {
                if self.now() >= until {
                    return;
                }
                if changed.changed().await.is_err() {
                    return;
                }
            }
        }
        tokio::time::sleep(until.saturating_duration_since(self.now())).await;
    }

    #[cfg(test)]
    fn scripted() -> Self {
        Self {
            clock: EngramBudgetClock::scripted(),
            changed: Arc::new(tokio::sync::watch::channel(0).0),
            io_deadlines: false,
        }
    }

    #[cfg(test)]
    fn scripted_io() -> Self {
        Self { io_deadlines: true, ..Self::scripted() }
    }

    #[cfg(test)]
    fn advance(&self, duration: Duration) {
        self.clock.advance(duration);
        self.changed.send_modify(|revision| *revision += 1);
    }
}
