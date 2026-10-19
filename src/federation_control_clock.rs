// Owns monotonic control deadlines and an explicit fixture clock.
// Does not change persistence, RPC limits or global Tokio time. New federation timing boundary.

#[derive(Clone)]
struct FederationControlClock {
    clock: EngramBudgetClock,
    #[cfg(test)]
    changed: Arc<tokio::sync::watch::Sender<u64>>,
}

impl Default for FederationControlClock {
    fn default() -> Self {
        Self {
            clock: EngramBudgetClock::Real,
            #[cfg(test)]
            changed: Arc::new(tokio::sync::watch::channel(0).0),
        }
    }
}

impl FederationControlClock {
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
        }
    }

    #[cfg(test)]
    fn advance(&self, duration: Duration) {
        self.clock.advance(duration);
        self.changed.send_modify(|revision| *revision += 1);
    }
}
