// Owns per-incarnation keys, replay reservation and bounded sender allocation.
// Does not perform IO or grant application authority. New A1 context leaf.

use super::*;

#[derive(Default)]
pub(crate) struct FederationReplayWindow {
    pub(crate) high: u64,
    bitmap: u64,
}

impl FederationReplayWindow {
    pub(crate) fn reserve(&mut self, counter: u64) -> Result<(), ApiError> {
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

/// The unchanged sender window. An unsettled request is given an UNKNOWN
/// outcome and its counter retired after this long. No timeout tears down the
/// authenticated link: the receiver's replay window still refuses a duplicate
/// or too-old counter.
pub(crate) const FEDERATION_SENDER_RETIREMENT: Duration = Duration::from_secs(10);

pub(crate) struct FederationSenderWindow {
    pub(crate) next: u64,
    pub(crate) outstanding: std::collections::BTreeMap<u64, FederationSenderDeadline>,
}

/// One unsettled counter, allocated by a fixed local operation.
pub(crate) struct FederationSenderDeadline {
    pub(crate) started: std::time::Instant,
    pub(crate) retire_at: std::time::Instant,
    pub(crate) operation: &'static str,
}

impl FederationSenderDeadline {
    pub(crate) fn new(now: std::time::Instant, operation: &'static str) -> Self {
        Self {
            started: now,
            retire_at: now + FEDERATION_SENDER_RETIREMENT,
            operation,
        }
    }
}

/// A bounded record of a retired request. It names only the fixed
/// operation, the counter and the elapsed time, never a payload or path.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FederationSenderEvent {
    pub(crate) kind: &'static str,
    pub(crate) operation: &'static str,
    pub(crate) counter: u64,
}

impl Default for FederationSenderWindow {
    fn default() -> Self {
        Self {
            next: 1,
            outstanding: Default::default(),
        }
    }
}

pub(crate) struct FederationSecurityContext {
    pub(crate) owner: u64,
    pub(crate) incarnation: String,
    pub(crate) transcript: Vec<u8>,
    pub(crate) keys: zeroize::Zeroizing<[[u8; 32]; 6]>,
    pub(crate) received: Mutex<FederationReplayWindow>,
    pub(crate) sender: Mutex<FederationSenderWindow>,
    pub(crate) changed: tokio::sync::Notify,
    pub(crate) watchdog_started: AtomicBool,
    // A satellite installs before central receives its final proof. Only a
    // valid central heartbeat proves the peer has installed this context too.
    pub(crate) outgoing_rpc_ready: AtomicBool,
    // Diagnostic facts stay with this exact context, including after replacement.
    pub(crate) installed_at: std::sync::OnceLock<std::time::Instant>,
    pub(crate) diagnostic_basis: std::sync::OnceLock<(u64, u64)>,
    pub(crate) diagnostic_attempt_basis: std::sync::OnceLock<(u64, u64)>,
    pub(crate) terminal_logged: AtomicBool,
    pub(crate) last_control_sequence: std::sync::atomic::AtomicU64,
    pub(crate) retired: tokio::sync::watch::Sender<bool>,
    #[cfg(test)]
    pub(crate) sender_events: Mutex<Vec<FederationSenderEvent>>,
}

impl FederationSecurityContext {
    pub(crate) fn derive(
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
            #[cfg(test)]
            sender_events: Mutex::default(),
        })
    }

    /// Settles a counter by its verified reply. Returns false if it was no
    /// longer outstanding, because it had already been retired as UNKNOWN: the
    /// caller then keeps that UNKNOWN outcome, so caller and record agree.
    pub(crate) fn settle(&self, counter: u64) -> bool {
        let settled = self
            .sender
            .lock()
            .expect("sender window poisoned")
            .outstanding
            .remove(&counter)
            .is_some();
        self.changed.notify_waiters();
        settled
    }

    /// Gives an unsettled counter an UNKNOWN outcome without touching the
    /// link, and records that once. Returns false if it had already settled
    /// or been retired, so a racing caller records nothing.
    pub(crate) fn retire_unknown(
        &self,
        counter: u64,
        reason: &'static str,
        now: std::time::Instant,
    ) -> bool {
        let Some(entry) = self
            .sender
            .lock()
            .expect("sender window poisoned")
            .outstanding
            .remove(&counter)
        else {
            return false;
        };
        // Record before waking waiters, so a woken waiter sees the record.
        self.record_sender_event(
            "retiredUnknown",
            reason,
            entry.operation,
            entry.started,
            counter,
            now,
        );
        self.changed.notify_waiters();
        true
    }

    fn record_sender_event(
        &self,
        kind: &'static str,
        reason: &'static str,
        operation: &'static str,
        started: std::time::Instant,
        counter: u64,
        now: std::time::Instant,
    ) {
        eprintln!(
            "federation request {kind} incarnation={} operation={operation} counter={counter} reason={reason} elapsedMs={} retireAfterMs={}",
            self.incarnation,
            now.saturating_duration_since(started).as_millis(),
            FEDERATION_SENDER_RETIREMENT.as_millis(),
        );
        #[cfg(test)]
        self.sender_events
            .lock()
            .expect("sender events poisoned")
            .push(FederationSenderEvent {
                kind,
                operation,
                counter,
            });
    }

    pub(crate) fn retire(&self) {
        self.retired.send_replace(true);
        self.changed.notify_waiters();
    }
}

impl FederationChannel {
    pub(crate) fn security_context(&self) -> Result<Arc<FederationSecurityContext>, ApiError> {
        self.state
            .lock()
            .expect("federation link mutex poisoned")
            .security
            .clone()
            .ok_or_else(federation_auth_refused)
    }

    pub(crate) fn security_current(
        &self,
        context: &Arc<FederationSecurityContext>,
    ) -> Result<(), ApiError> {
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

    pub(crate) fn fence_security(&self, owner: u64, incarnation: &str) {
        // Teardown can run during an earlier failure. Recover only to revoke
        // this exact owner; leave poison set so admission cannot reuse it.
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
