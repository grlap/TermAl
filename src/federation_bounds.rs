// Owns per-peer RPC pressure and reconnect delay, not identity, SSH or routes.
// New federation transport support; no source was split from another file.

#[derive(Clone)]
struct FederationRpcBound {
    permits: Arc<tokio::sync::Semaphore>,
}

impl Default for FederationRpcBound {
    fn default() -> Self {
        Self {
            permits: Arc::new(tokio::sync::Semaphore::new(4)),
        }
    }
}

impl FederationRpcBound {
    fn try_acquire(&self) -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
        self.permits.clone().try_acquire_owned().map_err(|_| {
            ApiError::from_status(
                StatusCode::TOO_MANY_REQUESTS,
                "federation peer RPC limit reached",
            )
        })
    }
}

#[derive(Default)]
struct FederationBackoff {
    failures: u32,
}

impl FederationBackoff {
    fn record_failure(&mut self) {
        self.failures = self.failures.saturating_add(1);
    }

    fn reset(&mut self) {
        self.failures = 0;
    }

    fn delay_for_entropy(&self, entropy: u64) -> Duration {
        let upper = (1000_u64 << self.failures.saturating_add(1).min(9)).min(300_000);
        let lower = (upper / 2).max(1000);
        let spread = upper - lower;
        let jitter = (u128::from(entropy) * u128::from(spread) / u128::from(u64::MAX)) as u64;
        Duration::from_millis(lower + jitter)
    }

    fn next_delay(&self) -> Duration {
        let random = Uuid::new_v4();
        let mut entropy = [0_u8; 8];
        entropy.copy_from_slice(&random.as_bytes()[..8]);
        self.delay_for_entropy(u64::from_le_bytes(entropy))
    }
}
