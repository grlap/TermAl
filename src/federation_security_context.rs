// Owns per-incarnation keys, replay reservation and bounded sender allocation.
// Does not perform IO or grant application authority. New A1 context leaf.

#[derive(Default)]
struct FederationReplayWindow {
    high: u64,
    bitmap: u64,
}

impl FederationReplayWindow {
    fn reserve(&mut self, counter: u64) -> Result<(), ApiError> {
        if counter == 0 {
            return Err(federation_auth_refused());
        }
        if counter > self.high {
            let shift = counter - self.high;
            self.bitmap = if shift >= 64 {
                1
            } else {
                (self.bitmap << shift) | 1
            };
            self.high = counter;
            return Ok(());
        }
        let distance = self.high - counter;
        if distance >= 64 || self.bitmap & (1_u64 << distance) != 0 {
            return Err(federation_auth_refused());
        }
        self.bitmap |= 1_u64 << distance;
        Ok(())
    }
}

struct FederationSenderWindow {
    next: u64,
    outstanding: std::collections::BTreeMap<u64, std::time::Instant>,
}

impl Default for FederationSenderWindow {
    fn default() -> Self {
        Self {
            next: 1,
            outstanding: Default::default(),
        }
    }
}

struct FederationSecurityContext {
    owner: u64,
    incarnation: String,
    transcript: Vec<u8>,
    keys: zeroize::Zeroizing<[[u8; 32]; 6]>,
    received: Mutex<FederationReplayWindow>,
    sender: Mutex<FederationSenderWindow>,
    changed: tokio::sync::Notify,
    watchdog_started: AtomicBool,
    // A satellite installs before central receives its final proof. Only a
    // valid central heartbeat proves the peer has installed this context too.
    outgoing_rpc_ready: AtomicBool,
    // Diagnostic facts stay with this exact context, including after replacement.
    installed_at: std::sync::OnceLock<std::time::Instant>,
    diagnostic_basis: std::sync::OnceLock<(u64, u64)>,
    diagnostic_attempt_basis: std::sync::OnceLock<(u64, u64)>,
    terminal_logged: AtomicBool,
    last_control_sequence: std::sync::atomic::AtomicU64,
    retired: tokio::sync::watch::Sender<bool>,
}

impl FederationSecurityContext {
    fn derive(
        owner: u64,
        incarnation: String,
        transcript: Vec<u8>,
        pair: &[u8],
        ns: &[u8],
        nc: &[u8],
    ) -> Result<Self, ApiError> {
        if ns.len() != 32 || nc.len() != 32 {
            return Err(federation_auth_refused());
        }
        let salt = [ns, nc].concat();
        let hkdf = hkdf::Hkdf::<Sha256>::new(Some(&salt), pair);
        let mut keys = zeroize::Zeroizing::new([[0; 32]; 6]);
        for (key, label) in keys.iter_mut().zip([
            "termal-link-req-c2s-v1",
            "termal-link-resp-c2s-v1",
            "termal-link-req-s2c-v1",
            "termal-link-resp-s2c-v1",
            "termal-link-ctl-c2s-v1",
            "termal-link-ctl-s2c-v1",
        ]) {
            let info = [label.as_bytes(), &transcript].concat();
            hkdf.expand(&info, key)
                .map_err(|_| federation_auth_refused())?;
        }
        Ok(Self {
            owner,
            incarnation,
            transcript,
            keys,
            received: Mutex::default(),
            sender: Mutex::default(),
            changed: Default::default(),
            watchdog_started: AtomicBool::new(false),
            outgoing_rpc_ready: AtomicBool::new(false),
            installed_at: std::sync::OnceLock::new(),
            diagnostic_basis: std::sync::OnceLock::new(),
            diagnostic_attempt_basis: std::sync::OnceLock::new(),
            terminal_logged: AtomicBool::new(false),
            last_control_sequence: std::sync::atomic::AtomicU64::new(0),
            retired: tokio::sync::watch::channel(false).0,
        })
    }

    fn settle(&self, counter: u64) {
        self.sender
            .lock()
            .expect("sender window poisoned")
            .outstanding
            .remove(&counter);
        self.changed.notify_waiters();
    }

    fn is_retired(&self) -> bool {
        *self.retired.borrow()
    }

    fn retire(&self) -> bool {
        let changed = self.retired.send_if_modified(|retired| {
            if *retired {
                return false;
            }
            *retired = true;
            true
        });
        if changed {
            self.changed.notify_waiters();
        }
        changed
    }
}

impl FederationChannel {
    fn authenticated_link_state(
        &self,
        active_test_runs: bool,
    ) -> Result<std::sync::MutexGuard<'_, FederationLinkState>, ApiError> {
        if active_test_runs {
            return self
                .state
                .try_lock()
                .map_err(|_| active_test_runs_unavailable());
        }
        Ok(self.state.lock().expect("federation link mutex poisoned"))
    }

    fn security_context(&self) -> Result<Arc<FederationSecurityContext>, ApiError> {
        self.security_context_for_rpc(false)
    }

    fn security_context_for_rpc(
        &self,
        active_test_runs: bool,
    ) -> Result<Arc<FederationSecurityContext>, ApiError> {
        self.authenticated_link_state(active_test_runs)?
            .security
            .as_ref()
            .filter(|context| !context.is_retired())
            .cloned()
            .ok_or_else(federation_auth_refused)
    }

    fn security_current(&self, context: &Arc<FederationSecurityContext>) -> Result<(), ApiError> {
        self.security_current_for_rpc(context, false)
    }

    fn security_current_for_rpc(
        &self,
        context: &Arc<FederationSecurityContext>,
        active_test_runs: bool,
    ) -> Result<(), ApiError> {
        let state = self.authenticated_link_state(active_test_runs)?;
        if context.is_retired() || state
            .security
            .as_ref()
            .is_none_or(|current| !Arc::ptr_eq(current, context))
            || state.incarnation.as_deref() != Some(context.incarnation.as_str())
            || state.recorded_incarnation.as_deref() != Some(context.incarnation.as_str())
        {
            return Err(federation_auth_refused());
        }
        Ok(())
    }

    fn fence_security(&self, owner: u64, incarnation: &str) {
        // Teardown can run during an earlier failure. Recover only to revoke
        // this exact owner; leave poison set so admission cannot reuse it.
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .security
            .as_ref()
            .is_some_and(|context| context.owner == owner && context.incarnation == incarnation)
        {
            state.incarnation = None;
            if let Some(context) = state.security.take() {
                context.retire();
            }
            self.retire_recorded(&mut state);
        }
    }
}
