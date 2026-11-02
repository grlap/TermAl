// Exact-generation ownership for an ordinary Engram connection transition.
// This fences connection replacement and child teardown, never turn admission
// on a remote control result. Observation preferences do not acquire it.

#[derive(Clone)]
struct EngramProjectResetClaim {
    generation: u64,
    #[cfg(test)]
    claimed_at: chrono::DateTime<chrono::Utc>,
    clock: EngramBudgetClock,
    deadline: std::time::Instant,
}

impl EngramProjectResetClaim {
    fn new(generation: u64, clock: EngramBudgetClock) -> Self {
        let deadline = clock.now() + Duration::from_millis(ENGRAM_MAX_CALL_TIMEOUT_MS + 1_000);
        Self {
            generation,
            #[cfg(test)]
            claimed_at: clock.wall_now(),
            clock,
            deadline,
        }
    }
}

impl StateInner {
    fn claim_engram_project_reset(&mut self, project_id: &str) -> Option<u64> {
        let clock = self.engram_budget_clock_snapshot();
        self.engram_project_resets
            .claim_with_clock(project_id, clock)
    }

    fn engram_project_reset_claim(
        &self,
        project_id: &str,
        generation: u64,
    ) -> Option<EngramProjectResetClaim> {
        self.engram_project_resets
            .claimed_at
            .get(project_id)
            .filter(|claim| claim.generation == generation)
            .cloned()
    }
}
