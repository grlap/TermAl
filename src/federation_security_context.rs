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

    fn retire(&self) {
        self.retired.send_replace(true);
        self.changed.notify_waiters();
    }
}

impl FederationChannel {
    fn security_context(&self) -> Result<Arc<FederationSecurityContext>, ApiError> {
        self.state
            .lock()
            .expect("federation link mutex poisoned")
            .security
            .clone()
            .ok_or_else(federation_auth_refused)
    }

    fn security_current(&self, context: &Arc<FederationSecurityContext>) -> Result<(), ApiError> {
        let state = self.state.lock().expect("federation link mutex poisoned");
        if state
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
        let mut state = self.state.lock().expect("federation link mutex poisoned");
        if state
            .security
            .as_ref()
            .is_some_and(|context| context.owner == owner && context.incarnation == incarnation)
        {
            self.retire_recorded(&mut state);
            state.incarnation = None;
            if let Some(context) = state.security.take() {
                context.retire();
            }
        }
    }
}
