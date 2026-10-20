// Owns bounded diagnostic retention of unresolved finite-read child handles.
// Does not wait, signal, retry, expose an API, or grant command admission.
// Split from the cleanup boundary in bounded_read_cleanup.rs.

#[cfg(any(unix, test))]
trait BoundedReadRetainedIdentity {
    fn same_owner(&self, other: &Self) -> bool;
}

#[cfg(test)]
impl<T> BoundedReadRetainedIdentity for Arc<T> {
    fn same_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(self, other)
    }
}

#[cfg(any(unix, test))]
struct RetainedBoundedRead<T> {
    pid: u32,
    sequence: u64,
    detail: String,
    retained: bool,
    _owner: T,
}

#[cfg(any(unix, test))]
struct BoundedReadRetainedOwners<T> {
    entries: VecDeque<RetainedBoundedRead<T>>,
    sequence: u64,
}

#[cfg(any(unix, test))]
impl<T> BoundedReadRetainedOwners<T> {
    const CAPACITY: usize = 32;

    fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            sequence: 0,
        }
    }

    fn insert(&mut self, pid: u32, owner: T, detail: String) -> Option<RetainedBoundedRead<T>>
    where
        T: BoundedReadRetainedIdentity,
    {
        // An unresolved reaped owner can arrive after a newer child reused
        // its PID. Refresh only this exact owner; other incarnations remain
        // retained until exact removal or an explicitly reported eviction.
        self.entries
            .retain(|entry| entry.pid != pid || !entry._owner.same_owner(&owner));
        self.sequence = self.sequence.saturating_add(1);
        let evicted = if self.entries.len() == Self::CAPACITY {
            self.entries.pop_front().map(|mut entry| {
                entry.retained = false;
                entry
            })
        } else {
            None
        };
        self.entries.push_back(RetainedBoundedRead {
            pid,
            sequence: self.sequence,
            detail,
            retained: true,
            _owner: owner,
        });
        evicted
    }

    fn remove_reaped_matching(&mut self, pid: u32, matches: impl Fn(&T) -> bool) {
        self.entries
            .retain(|entry| entry.pid != pid || !matches(&entry._owner));
    }

    #[cfg(test)]
    fn remove_reaped(&mut self, pid: u32) {
        self.remove_reaped_matching(pid, |_| true);
    }
}

#[cfg(unix)]
struct RetainedUnixReadOwner {
    process: Arc<SharedChild>,
    // A later explicit reconciliation must preserve this phase: no group
    // signal is legal once the leader was reaped, even under the same PID.
    reaped: bool,
}

#[cfg(unix)]
impl BoundedReadRetainedIdentity for RetainedUnixReadOwner {
    fn same_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.process, &other.process)
    }
}

#[cfg(unix)]
fn unresolved_bounded_reads() -> &'static Mutex<BoundedReadRetainedOwners<RetainedUnixReadOwner>> {
    static OWNERS: OnceLock<Mutex<BoundedReadRetainedOwners<RetainedUnixReadOwner>>> =
        OnceLock::new();
    OWNERS.get_or_init(|| Mutex::new(BoundedReadRetainedOwners::new()))
}

#[cfg(unix)]
fn retain_unresolved_bounded_read(process: Arc<SharedChild>, reaped: bool, detail: String) {
    let pid = process.id();
    let evicted = unresolved_bounded_reads()
        .lock()
        .expect("read owner mutex poisoned")
        .insert(pid, RetainedUnixReadOwner { process, reaped }, detail);
    if let Some(entry) = evicted {
        // SharedChild/Child Drop does not wait or signal. The unreaped child,
        // not this Arc, pins its PID; eviction never touches the numeric group.
        eprintln!(
            "bounded read> pid={} sequence={} reaped={} retained={}: {}",
            entry.pid, entry.sequence, entry._owner.reaped, entry.retained, entry.detail
        );
    }
}

#[cfg(unix)]
fn remove_reaped_bounded_read(process: &Arc<SharedChild>) {
    unresolved_bounded_reads()
        .lock()
        .expect("read owner mutex poisoned")
        // PID reuse after wait must not remove a newer retained child's row.
        .remove_reaped_matching(process.id(), |owner| Arc::ptr_eq(&owner.process, process));
}
