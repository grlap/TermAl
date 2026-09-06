/*
Content-specific persist acknowledgements.
Owns fence identity, completion and worker batching alongside app_boot.rs and
persist.rs. This is a new boundary, not a moved implementation. It does not own
delegation admission, provider delivery, SQL schema, or HTTP response policy.
*/

/// Equality is deliberately about materialized content, not the global
/// revision or the collector watermark (which can advance past deferred rows).
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone)]
enum PersistFenceTarget {
    Delegation(Box<DelegationRecord>),
    WaitRegistration(DelegationWaitRecord),
}

impl PersistFenceTarget {
    fn is_in_delta(&self, delta: &PersistDelta) -> bool {
        match self {
            Self::Delegation(expected) => delta
                .changed_delegations
                .as_deref()
                .unwrap_or_default()
                .iter()
                .any(|actual| actual == expected.as_ref()),
            Self::WaitRegistration(expected) => delta
                .metadata
                .delegation_waits
                .iter()
                .any(|actual| actual == expected),
        }
    }

    /// A request can arrive after the worker drained its channel but before
    /// that tick committed this content. Its next delta then omits the stable
    /// row. Verify the stored bytes on the SAME cached writer connection; do
    /// not open another connection or treat row existence as proof.
    fn is_already_durable(&self, connection: &rusqlite::Connection) -> Result<bool> {
        match self {
            Self::Delegation(expected) => {
                let stored: Option<String> = connection
                    .query_row(
                        "SELECT value_json FROM delegations WHERE id = ?1",
                        [&expected.id],
                        |row| row.get(0),
                    )
                    .optional()
                    .context("failed to verify persisted delegation fence")?;
                let expected_json = serde_json::to_string(expected.as_ref())
                    .context("failed to serialize delegation fence target")?;
                Ok(stored.as_deref() == Some(expected_json.as_str()))
            }
            // Every delta writes the whole metadata document, including waits.
            // Absence/mismatch there is already conclusive for this tick.
            Self::WaitRegistration(_) => Ok(false),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PersistFenceError {
    /// Acceptance is uncertain if the deadline raced an in-flight commit.
    Deadline,
    WriteFailed(String),
    Shutdown,
    WorkerStopped,
}

type PersistFenceResult = std::result::Result<(), PersistFenceError>;

struct PersistFenceCompletion {
    result: Mutex<Option<PersistFenceResult>>,
    changed: Condvar,
    deadline: std::time::Instant,
}

impl PersistFenceCompletion {
    fn resolve_at(&self, result: PersistFenceResult, now: std::time::Instant) {
        let mut slot = self.result.lock().expect("persist fence mutex poisoned");
        if slot.is_none() {
            *slot = Some(if now >= self.deadline {
                Err(PersistFenceError::Deadline)
            } else {
                result
            });
            self.changed.notify_all();
        }
    }

    fn poll_at(&self, now: std::time::Instant) -> Option<PersistFenceResult> {
        let mut slot = self.result.lock().expect("persist fence mutex poisoned");
        if slot.is_none() && now >= self.deadline {
            *slot = Some(Err(PersistFenceError::Deadline));
            self.changed.notify_all();
        }
        slot.clone()
    }
}

struct PersistFence {
    target: PersistFenceTarget,
    completion: Arc<PersistFenceCompletion>,
}

#[cfg_attr(not(test), allow(dead_code))]
struct PersistFenceWaiter {
    completion: Arc<PersistFenceCompletion>,
}

impl PersistFence {
    #[cfg_attr(not(test), allow(dead_code))]
    fn new(target: PersistFenceTarget, deadline: std::time::Instant) -> (Self, PersistFenceWaiter) {
        let completion = Arc::new(PersistFenceCompletion {
            result: Mutex::new(None),
            changed: Condvar::new(),
            deadline,
        });
        (
            Self {
                target,
                completion: completion.clone(),
            },
            PersistFenceWaiter { completion },
        )
    }

    fn finish(&self, result: PersistFenceResult) {
        self.completion.resolve_at(result, std::time::Instant::now());
    }
}

impl Drop for PersistFence {
    fn drop(&mut self) {
        // Covers a disconnected request channel, worker unwind, and a batch
        // abandoned before writing. An already terminal result never changes.
        self.finish(Err(PersistFenceError::WorkerStopped));
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl PersistFenceWaiter {
    /// Blocking boundary: the caller must release StateInner before entering.
    fn wait(self) -> PersistFenceResult {
        let mut slot = self
            .completion
            .result
            .lock()
            .expect("persist fence mutex poisoned");
        loop {
            if let Some(result) = slot.as_ref() {
                return result.clone();
            }
            let remaining = self
                .completion
                .deadline
                .saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                *slot = Some(Err(PersistFenceError::Deadline));
                self.completion.changed.notify_all();
                return Err(PersistFenceError::Deadline);
            }
            (slot, _) = self
                .completion
                .changed
                .wait_timeout(slot, remaining)
                .expect("persist fence mutex poisoned");
        }
    }
}

#[derive(Default)]
struct PersistFenceBatch {
    pending: Vec<PersistFence>,
}

impl PersistFenceBatch {
    fn accept(&mut self, request: PersistRequest) -> PersistWorkerWaitOutcome {
        match request {
            PersistRequest::Delta => PersistWorkerWaitOutcome::Process,
            PersistRequest::Shutdown => PersistWorkerWaitOutcome::Shutdown,
            PersistRequest::Fence(fence) => {
                self.pending.push(*fence);
                PersistWorkerWaitOutcome::Process
            }
        }
    }

    fn drain(&mut self, rx: &mpsc::Receiver<PersistRequest>, mut shutdown: bool) -> bool {
        while let Ok(request) = rx.try_recv() {
            shutdown |= matches!(self.accept(request), PersistWorkerWaitOutcome::Shutdown);
        }
        shutdown
    }

    fn fail(&mut self, error: PersistFenceError) {
        for fence in self.pending.drain(..) {
            fence.finish(Err(error.clone()));
        }
    }

    fn expire_resolved(&mut self) {
        self.pending.retain(|fence| {
            fence.completion.poll_at(std::time::Instant::now()).is_none()
        });
    }

    fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    fn finish_write(
        &mut self,
        delta: &PersistDelta,
        result: &Result<Vec<String>>,
        connection: Option<&rusqlite::Connection>,
    ) {
        if let Err(error) = result {
            self.fail(PersistFenceError::WriteFailed(format!("{error:#}")));
            return;
        }
        self.pending.retain(|fence| {
            if fence.completion.poll_at(std::time::Instant::now()).is_some() {
                return false;
            }
            let proof = if fence.target.is_in_delta(delta) {
                Ok(true)
            } else if let Some(connection) = connection {
                fence.target.is_already_durable(connection)
            } else {
                Ok(false)
            };
            match proof {
                Ok(true) => fence.finish(Ok(())),
                Ok(false) => return true,
                Err(error) => {
                    // The write itself committed; a read-back error fails this
                    // fence, not the worker's successful mutation watermark.
                    fence.finish(Err(PersistFenceError::WriteFailed(format!("{error:#}"))));
                }
            }
            false
        });
    }
}

fn persist_delta_with_fences(
    cache: &mut SqlitePersistConnectionCache,
    path: &FsPath,
    delta: &PersistDelta,
    fences: &mut PersistFenceBatch,
) -> Result<Vec<String>> {
    fences.expire_resolved();
    let result = persist_delta_via_cache(cache, path, delta);
    // persist_delta_via_cache returns only after COMMIT and the existing
    // post-commit integrity checks. No StateInner guard is held here.
    fences.finish_write(delta, &result, cache.connection.as_ref());
    result
}

#[cfg(test)]
fn persist_delta_with_fences_using(
    delta: &PersistDelta,
    fences: &mut PersistFenceBatch,
    write: impl FnOnce() -> Result<Vec<String>>,
) -> Result<Vec<String>> {
    let result = write();
    fences.finish_write(delta, &result, None);
    result
}
