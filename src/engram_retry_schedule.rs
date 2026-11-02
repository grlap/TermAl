// Non-blocking verification writers. These services never admit a turn,
// retry a control operation, or hold ordinary dispatch and queue progression.

#[cfg(not(test))]
const ENGRAM_VERIFICATION_TICK_INTERVAL: Duration = Duration::from_secs(2);

impl AppState {
    #[cfg(not(test))]
    fn spawn_engram_verification_ticks(&self) {
        let state = self.clone();
        let shutdown = self.subscribe_shutdown_signal();
        let spawned = std::thread::Builder::new()
            .name("termal-engram-verification-records".to_owned())
            .spawn(move || {
                let mut cursor = EngramVerificationRecordCursor::default();
                while !*shutdown.borrow() {
                    state.engram_verification_records_tick_from(
                        ENGRAM_VERIFICATION_RECORD_TICK_BUDGET,
                        &|| *shutdown.borrow(),
                        &mut cursor,
                    );
                    std::thread::sleep(ENGRAM_VERIFICATION_TICK_INTERVAL);
                }
            });
        if let Err(err) = spawned {
            eprintln!("engram> failed to start the verification records tick: {err}");
        }
    }
}
