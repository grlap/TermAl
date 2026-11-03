// Process-local ownership of read-only capture workers. Caller timeouts do not
// retire workers: the host retains their handles and cancels them at shutdown.
#[derive(Default)]
struct CaptureLifetime {
    state: Mutex<CaptureLifetimeState>,
    changed: std::sync::Condvar,
    #[cfg(test)]
    root: Mutex<Option<Arc<TestTempRoot>>>,
    #[cfg(test)]
    before_capture: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    expire_shutdown: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    before_shutdown_wait: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

#[derive(Default)]
struct CaptureLifetimeState {
    stopping: bool,
    pending_logged: bool,
    draining: Option<Arc<std::sync::atomic::AtomicBool>>,
    workers: Vec<CaptureWorkerHandle>,
}

struct CaptureWorkerHandle {
    thread: std::thread::JoinHandle<()>,
    cancel: Arc<CaptureCancellation>,
    done: Arc<std::sync::atomic::AtomicBool>,
}

struct CaptureCancellation {
    deadline: Option<std::time::Instant>,
    state: Mutex<CaptureCancellationState>,
    cleanup_changed: std::sync::Condvar,
    #[cfg(any(unix, windows))]
    owner: std::sync::Weak<CaptureLifetime>,
}

#[derive(Default)]
struct CaptureCancellationState {
    stopped: bool,
    next: u64,
    children: HashMap<u64, Arc<dyn Fn() + Send + Sync>>,
    // Unresolved cleanup outlives its capture thread and the bounded diagnostic
    // table. Only a complete group-cleanup certificate releases this custody.
    #[cfg(unix)]
    pending_reads: HashMap<usize, Arc<SharedChild>>,
    #[cfg(windows)]
    pending_jobs: HashMap<usize, Arc<windows_launch::CaptureJob>>,
}

thread_local! {
    // Installed only while an owned worker runs. Nested read helpers inherit
    // the same absolute budget and cancellation, including acceptance reads.
    static CAPTURE_CANCELLATION: std::cell::RefCell<Option<Arc<CaptureCancellation>>> = const {
        std::cell::RefCell::new(None)
    };
}

struct CaptureCancellationScope(Option<Arc<CaptureCancellation>>);
impl CaptureCancellationScope {
    fn enter(cancel: Arc<CaptureCancellation>) -> Self {
        Self(CAPTURE_CANCELLATION.with(|slot| slot.replace(Some(cancel))))
    }
}
impl Drop for CaptureCancellationScope {
    fn drop(&mut self) {
        CAPTURE_CANCELLATION.with(|slot| slot.replace(self.0.take()));
    }
}

fn current_capture_cancellation() -> Option<Arc<CaptureCancellation>> {
    CAPTURE_CANCELLATION.with(|slot| slot.borrow().clone())
}

fn check_capture_cancellation() -> Result<()> {
    if let Some(cancel) = current_capture_cancellation() {
        if cancel.state.lock().unwrap().stopped {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "capture cancelled by host shutdown",
            )
            .into());
        }
        if cancel
            .deadline
            .is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "capture shared deadline exceeded",
            )
            .into());
        }
    }
    Ok(())
}

fn capture_deadline(deadline: std::time::Instant) -> std::time::Instant {
    current_capture_cancellation()
        .and_then(|cancel| cancel.deadline)
        .map_or(deadline, |limit| deadline.min(limit))
}

struct CaptureChildRegistration {
    cancel: Arc<CaptureCancellation>,
    id: u64,
}
impl Drop for CaptureChildRegistration {
    fn drop(&mut self) {
        self.cancel.state.lock().unwrap().children.remove(&self.id);
    }
}

// Register before resuming a suspended Windows child or waiting on a Unix
// child. A stop racing process creation kills the new child immediately.
fn register_capture_child(
    kill: impl Fn() + Send + Sync + 'static,
) -> Option<CaptureChildRegistration> {
    let cancel = current_capture_cancellation()?;
    let kill: Arc<dyn Fn() + Send + Sync> = Arc::new(kill);
    let (id, stopped) = {
        let mut state = cancel.state.lock().unwrap();
        state.next += 1;
        let id = state.next;
        state.children.insert(id, kill.clone());
        (id, state.stopped)
    };
    if stopped {
        kill();
    }
    Some(CaptureChildRegistration { cancel, id })
}

impl CaptureCancellation {
    fn cleanup_pending(&self) -> bool {
        #[cfg(unix)]
        {
            !self.state.lock().unwrap().pending_reads.is_empty()
        }
        #[cfg(windows)]
        {
            !self.state.lock().unwrap().pending_jobs.is_empty()
        }
        #[cfg(not(any(unix, windows)))]
        {
            false
        }
    }

    #[cfg(windows)]
    fn retain_job(self: &Arc<Self>, job: Arc<windows_launch::CaptureJob>) {
        let id = Arc::as_ptr(&job) as usize;
        if self.state.lock().unwrap().pending_jobs.insert(id, job.clone()).is_some() {
            return;
        }
        // The bounded worker can finish, but its job and root cannot. A late
        // certificate alone releases this custody. Native errors retain it.
        let retained = Arc::new(Mutex::new(Some((self.owner.upgrade(), self.clone(), job))));
        let draining = retained.clone();
        if let Err(error) = std::thread::Builder::new().name("termal-capture-job-drain".into())
            .spawn(move || {
                let (owner, cancel, job) = draining.lock().unwrap().take().unwrap();
                match job.finish(None) {
                    Ok(true) => {
                        {
                            let mut state = cancel.state.lock().unwrap();
                            state.pending_jobs.remove(&id);
                            cancel.cleanup_changed.notify_all();
                        }
                        if let Some(owner) = &owner {
                            let _state = owner.state.lock().unwrap();
                            owner.changed.notify_all();
                        }
                    }
                    outcome => {
                        eprintln!("capture_cleanup_pending Windows job certificate: {outcome:?}");
                        std::mem::forget((owner, cancel, job));
                    }
                }
            }) {
            eprintln!("capture_cleanup_pending Windows job drainer: {error}");
            std::mem::forget(retained);
        }
    }

    #[cfg(unix)]
    fn retain_read(&self, process: Arc<SharedChild>) {
        self.state
            .lock()
            .unwrap()
            .pending_reads
            .insert(Arc::as_ptr(&process) as usize, process);
    }

    #[cfg(unix)]
    fn complete_read(&self, process: &Arc<SharedChild>) {
        {
            let mut state = self.state.lock().unwrap();
            state.pending_reads.remove(&(Arc::as_ptr(process) as usize));
            self.cleanup_changed.notify_all();
        }
        if let Some(owner) = self.owner.upgrade() {
            // Pair with the registry lock used by shutdown's predicate/wait.
            let _state = owner.state.lock().unwrap();
            owner.changed.notify_all();
        }
    }

    fn wait_for_cleanup(&self) {
        #[cfg(unix)]
        {
            let mut state = self.state.lock().unwrap();
            while !state.pending_reads.is_empty() {
                state = self.cleanup_changed.wait(state).unwrap();
            }
        }
        #[cfg(windows)]
        {
            let mut state = self.state.lock().unwrap();
            while !state.pending_jobs.is_empty() {
                state = self.cleanup_changed.wait(state).unwrap();
            }
        }
        #[cfg(not(any(unix, windows)))]
        let _ = &self.cleanup_changed;
    }

    fn stop(&self) {
        let children = {
            let mut state = self.state.lock().unwrap();
            state.stopped = true;
            state.children.values().cloned().collect::<Vec<_>>()
        };
        // Process termination never holds the admission or registry mutex.
        for kill in children {
            kill();
        }
    }
}

impl CaptureLifetime {
    #[cfg(test)]
    fn with_test_root(root: Option<Arc<TestTempRoot>>) -> Self {
        Self {
            root: Mutex::new(root),
            ..Self::default()
        }
    }

    fn spawn(self: &Arc<Self>, name: &str, run: impl FnOnce() + Send + 'static) -> io::Result<()> {
        self.spawn_owned(
            name,
            Some(std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT),
            run,
        )
    }

    // Delivery is a long-lived loop, not one capture. Its nested captures get
    // their own existing budget; shutdown still cancels its in-flight child.
    fn spawn_observer(
        self: &Arc<Self>,
        name: &str,
        run: impl FnOnce() + Send + 'static,
    ) -> io::Result<()> {
        self.spawn_owned(name, None, run)
    }

    fn spawn_owned(
        self: &Arc<Self>,
        name: &str,
        deadline: Option<std::time::Instant>,
        run: impl FnOnce() + Send + 'static,
    ) -> io::Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.stopping {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "capture host is stopping",
            ));
        }
        let mut finished = Vec::new();
        let mut index = 0;
        while index < state.workers.len() {
            if state.workers[index].thread.is_finished()
                && !state.workers[index].cancel.cleanup_pending()
            {
                finished.push(state.workers.swap_remove(index).thread);
            } else {
                index += 1;
            }
        }
        let cancel = Arc::new(CaptureCancellation {
            deadline,
            state: Mutex::new(CaptureCancellationState::default()),
            cleanup_changed: std::sync::Condvar::new(),
            #[cfg(any(unix, windows))]
            owner: Arc::downgrade(self),
        });
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let owner = self.clone();
        let worker_cancel = cancel.clone();
        let worker_done = done.clone();
        #[cfg(test)]
        let before_capture = self.before_capture.lock().unwrap().clone();
        let spawned = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _scope = CaptureCancellationScope::enter(worker_cancel);
                    #[cfg(test)]
                    if let Some(hook) = before_capture {
                        hook();
                    }
                    if check_capture_cancellation().is_ok() {
                        run();
                    }
                }));
                // All capture resources and the TLS scope have dropped before the
                // completion publication. The registry mutex is never needed by
                // a joining thread: Condvar::wait_timeout releases it.
                let _state = owner.state.lock().unwrap();
                worker_done.store(true, std::sync::atomic::Ordering::Release);
                owner.changed.notify_all();
                drop(_state);
                if let Err(panic) = outcome {
                    std::panic::resume_unwind(panic);
                }
            });
        let result = match spawned {
            Ok(thread) => {
                state.workers.push(CaptureWorkerHandle {
                    thread,
                    cancel,
                    done,
                });
                Ok(())
            }
            Err(error) => Err(error),
        };
        drop(state);
        for thread in finished {
            let _ = thread.join();
        }
        result
    }

    fn stop(&self) {
        let workers = {
            let mut state = self.state.lock().unwrap();
            state.stopping = true;
            state
                .workers
                .iter()
                .map(|worker| worker.cancel.clone())
                .collect::<Vec<_>>()
        };
        for worker in workers {
            worker.stop();
        }
    }

    /// Returns false only for an uninterruptible read/failed termination. The
    /// retained drainer owns handles and root custody until actual completion;
    /// the exiting host never waits beyond the workers' existing shared budget.
    fn stop_and_join(self: &Arc<Self>) -> bool {
        self.stop();
        let backstop = std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT;
        let mut state = self.state.lock().unwrap();
        loop {
            // A different waiter may have transferred all handles to a drainer
            // while this waiter released the mutex in the condvar wait.
            if let Some(draining) = &state.draining {
                return draining.load(std::sync::atomic::Ordering::Acquire);
            }
            if !state.workers.iter().any(|worker| {
                !worker.done.load(std::sync::atomic::Ordering::Acquire)
                    || worker.cancel.cleanup_pending()
            }) {
                break;
            }
            let deadline = state
                .workers
                .iter()
                .map(|worker| worker.cancel.deadline.unwrap_or(backstop))
                .max()
                .unwrap();
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            #[cfg(test)]
            let remaining = if self
                .expire_shutdown
                .load(std::sync::atomic::Ordering::Acquire)
            {
                Duration::ZERO
            } else {
                remaining
            };
            if remaining.is_zero() {
                let count = state
                    .workers
                    .iter()
                    .filter(|worker| {
                        !worker.done.load(std::sync::atomic::Ordering::Acquire)
                            || worker.cancel.cleanup_pending()
                    })
                    .count();
                let children: usize = state
                    .workers
                    .iter()
                    .map(|worker| worker.cancel.state.lock().unwrap().children.len())
                    .sum();
                if !state.pending_logged {
                    eprintln!("capture_cleanup_pending workers={count} children={children}");
                    state.pending_logged = true;
                }
                // Keep the complete lifetime (including root leases) alive.
                // This dedicated drainer may block; shutdown does not join it.
                let drained = Arc::new(std::sync::atomic::AtomicBool::new(false));
                state.draining = Some(drained.clone());
                self.changed.notify_all();
                let owner = self.clone();
                let workers = std::mem::take(&mut state.workers);
                drop(state);
                // Spawn failure must retain the handles, not drop custody.
                let retained = Arc::new(Mutex::new(Some((owner, workers))));
                let draining = retained.clone();
                if let Err(error) = std::thread::Builder::new()
                    .name("termal-capture-drain".to_owned())
                    .spawn(move || {
                        let (owner, workers) = draining.lock().unwrap().take().unwrap();
                        for worker in workers {
                            let _ = worker.thread.join();
                            worker.cancel.wait_for_cleanup();
                        }
                        drained.store(true, std::sync::atomic::Ordering::Release);
                        drop(owner);
                    })
                {
                    eprintln!("capture_cleanup_pending drainer unavailable: {error}");
                    std::mem::forget(retained);
                }
                return false;
            }
            #[cfg(test)]
            if let Some(hook) = self.before_shutdown_wait.lock().unwrap().clone() {
                hook();
            }
            state = self.changed.wait_timeout(state, remaining).unwrap().0;
        }
        let workers = std::mem::take(&mut state.workers);
        drop(state);
        for worker in workers {
            let _ = worker.thread.join();
        }
        true
    }
}
