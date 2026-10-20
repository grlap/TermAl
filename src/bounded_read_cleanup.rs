// Owns the finite read worker's injectable group-signal/reap/probe boundary.
// Does not own terminal commands, authority, or Windows Job Object cleanup.
// Extracted from bounded_read_process.rs; the worker remains the sole waiter.

#[cfg(any(unix, test))]
fn admit_bounded_read_group<T>(
    process_id: u32,
    admission: Result<T>,
    retain: impl FnOnce(String),
) -> Result<T> {
    admission.map_err(|error| {
        // The caller has committed to this cleanup attempt. Even a failure
        // before its first syscall must retain the owner for diagnostics.
        let message = format!(
            "bounded read cleanup unresolved: pid={process_id} reason=admission \
             signal_errno=None probe_errno=None probe_attempted=false detail={error:#}"
        );
        retain(message.clone());
        error.context(message)
    })
}

#[cfg(any(unix, test))]
trait BoundedReadGroupIo {
    type Status;

    fn signal_before_reap(&mut self) -> io::Result<()>;
    fn observe_before_reap(&mut self) -> io::Result<bool>;
    fn reap(&mut self) -> io::Result<Self::Status>;
    fn probe_after_reap(&mut self) -> io::Result<()>;
    fn retry_probe(&mut self) -> bool;
    fn missing_errno(&self) -> i32;
    fn permission_errno(&self) -> i32;
    fn process_id(&self) -> u32;
    fn retain_unresolved(&mut self, detail: String);
}

#[cfg(any(unix, test))]
fn finish_bounded_read_group<I: BoundedReadGroupIo>(ops: &mut I) -> Result<I::Status> {
    // Only this first syscall can send a non-zero signal. Neither EPERM
    // (including Darwin's zombie-only result) nor ESRCH is confirmation.
    let signal_error = ops.signal_before_reap().err();
    loop {
        match ops.observe_before_reap() {
            Ok(true) => break,
            Ok(false) if ops.retry_probe() => {}
            Ok(false) => {
                return Err(unresolved_read_group(
                    ops,
                    "leader_alive",
                    signal_error.as_ref(),
                    None,
                    false,
                    None,
                ));
            }
            Err(error) => {
                return Err(unresolved_read_group(
                    ops,
                    "observe",
                    signal_error.as_ref(),
                    None,
                    false,
                    Some(&error),
                ));
            }
        }
    }
    let status = match ops.reap() {
        Ok(status) => status,
        Err(error) => {
            return Err(unresolved_read_group(
                ops,
                "reap",
                signal_error.as_ref(),
                None,
                false,
                Some(&error),
            ));
        }
    };
    let signal_known = signal_error.as_ref().is_none_or(|error| {
        error.raw_os_error() == Some(ops.missing_errno())
            || error.raw_os_error() == Some(ops.permission_errno())
    });
    loop {
        // Always make one harmless probe after reaping, even if observation
        // consumed the whole budget. A reused group produces uncertainty.
        let probe_error = ops.probe_after_reap().err();
        if probe_error.as_ref().and_then(io::Error::raw_os_error) == Some(ops.missing_errno()) {
            return if signal_known {
                Ok(status)
            } else {
                Err(unresolved_read_group(
                    ops,
                    "signal",
                    signal_error.as_ref(),
                    probe_error.as_ref(),
                    true,
                    None,
                ))
            };
        }
        if probe_error
            .as_ref()
            .is_some_and(|error| error.raw_os_error() != Some(ops.permission_errno()))
        {
            return Err(unresolved_read_group(
                ops,
                "probe",
                signal_error.as_ref(),
                probe_error.as_ref(),
                true,
                None,
            ));
        }
        if !ops.retry_probe() {
            let permission = signal_error.as_ref().and_then(io::Error::raw_os_error)
                == Some(ops.permission_errno())
                || probe_error.as_ref().and_then(io::Error::raw_os_error)
                    == Some(ops.permission_errno());
            return Err(unresolved_read_group(
                ops,
                if permission {
                    "permission"
                } else {
                    "group_present"
                },
                signal_error.as_ref(),
                probe_error.as_ref(),
                true,
                None,
            ));
        }
    }
}

#[cfg(any(unix, test))]
fn unresolved_read_group<I: BoundedReadGroupIo>(
    ops: &mut I,
    reason: &str,
    signal: Option<&io::Error>,
    probe: Option<&io::Error>,
    probe_attempted: bool,
    detail: Option<&io::Error>,
) -> anyhow::Error {
    let message = format!(
        "bounded read cleanup unresolved: pid={} reason={reason} signal_errno={:?} \
         probe_errno={:?} probe_attempted={probe_attempted} signal_error={:?} \
         probe_error={:?} detail={:?}",
        ops.process_id(),
        signal.and_then(io::Error::raw_os_error),
        probe.and_then(io::Error::raw_os_error),
        signal.map(ToString::to_string),
        probe.map(ToString::to_string),
        detail.map(ToString::to_string),
    );
    ops.retain_unresolved(message.clone());
    anyhow!(message)
}

#[cfg(unix)]
struct UnixBoundedReadGroup<'a> {
    process: &'a Arc<SharedChild>,
    reaped: &'a mut bool,
    reap_state: BoundedReadReapState,
    pgid: libc::pid_t,
    probe_deadline: std::time::Instant,
}

#[cfg(unix)]
impl BoundedReadGroupIo for UnixBoundedReadGroup<'_> {
    type Status = std::process::ExitStatus;

    fn signal_before_reap(&mut self) -> io::Result<()> {
        // SharedChild::spawn and WNOWAIT preserve the leader's PID until
        // this worker's reap. Never add a wait thread at this boundary.
        if *self.reaped || !self.reap_state.can_signal() {
            return Err(io::Error::other("refusing group signal after leader wait attempt"));
        }
        if unsafe { libc::killpg(self.pgid, libc::SIGKILL) } == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn reap(&mut self) -> io::Result<Self::Status> {
        let status = self.reap_state.wait(|| self.process.wait())?;
        *self.reaped = true;
        remove_reaped_bounded_read(self.process);
        Ok(status)
    }

    fn observe_before_reap(&mut self) -> io::Result<bool> {
        let observed = read_child_has_exited(self.process);
        if observed.is_err() {
            self.reap_state = BoundedReadReapState::Unknown;
        }
        observed
    }

    fn probe_after_reap(&mut self) -> io::Result<()> {
        // A reused numeric group may only be observed, never signalled.
        if !*self.reaped || self.reap_state != BoundedReadReapState::Reaped {
            return Err(io::Error::other("refusing group probe before leader reap"));
        }
        if unsafe { libc::kill(-self.pgid, 0) } == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn retry_probe(&mut self) -> bool {
        let remaining = self
            .probe_deadline
            .saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        std::thread::sleep(remaining.min(Duration::from_millis(10)));
        std::time::Instant::now() < self.probe_deadline
    }

    fn missing_errno(&self) -> i32 {
        libc::ESRCH
    }

    fn permission_errno(&self) -> i32 {
        libc::EPERM
    }

    fn process_id(&self) -> u32 {
        self.process.id()
    }

    fn retain_unresolved(&mut self, detail: String) {
        retain_unresolved_bounded_read(self.process.clone(), self.reap_state, detail);
    }
}
