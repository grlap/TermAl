// Bounded advisory evidence for the typed StateMutex, never lock authority.
// StateInner -> atomic active/release publication; unlock -> try-lock fixed ring.
// Readers copy the ring off StateInner and format off both locks. Every point
// here shares one process origin, distinct from the phase-trace clock. Failed
// try-lock probes have no span. Missing samples and scheduling time are not causes.
const STATE_LOCK_HISTORY: usize = 256;
static STATE_LOCK_IDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
static STATE_LOCK_EPOCHS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
static STATE_LOCK_ORIGIN: LazyLock<std::time::Instant> = LazyLock::new(std::time::Instant::now);
#[cfg(test)]
static STATE_LOCK_PROCESS: LazyLock<String> = LazyLock::new(|| Uuid::new_v4().to_string());

#[cfg(test)]
thread_local! {
    // One-shot scheduling only: production counters and history supply evidence.
    static TEST_STATE_LOCK_COUNTERS_SAMPLED: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

fn next_state_lock_epoch() -> u64 {
    STATE_LOCK_EPOCHS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn state_lock_point(at: std::time::Instant) -> u64 {
    at.saturating_duration_since(*STATE_LOCK_ORIGIN)
        .as_micros()
        .min(u128::from(u64::MAX)) as u64
}

fn state_lock_value_epoch<T: 'static>(value: &T, fallback: u64) -> u64 {
    (value as &dyn std::any::Any)
        .downcast_ref::<StateInner>()
        .map_or(fallback, |inner| inner.state_lock_epoch)
}

#[derive(Clone, Copy)]
struct StateLockSpan {
    id: u64,
    epoch: u64,
    site: &'static std::panic::Location<'static>,
    requested: u64,
    acquired: u64,
    released: Option<u64>,
}

struct StateLockRing {
    slots: [Option<StateLockSpan>; STATE_LOCK_HISTORY],
    next: usize,
    count: usize,
}

struct StateLockObserver {
    instance: String,
    fallback_epoch: u64,
    active_id: std::sync::atomic::AtomicU64,
    active_epoch: std::sync::atomic::AtomicU64,
    active_site: std::sync::atomic::AtomicPtr<std::panic::Location<'static>>,
    active_requested: std::sync::atomic::AtomicU64,
    active_acquired: std::sync::atomic::AtomicU64,
    ring: Mutex<StateLockRing>,
    loss: std::sync::atomic::AtomicU64,
    evictions: std::sync::atomic::AtomicU64,
    changing: std::sync::atomic::AtomicU64,
    generation: std::sync::atomic::AtomicU64,
}

impl StateLockObserver {
    fn new(instance: &str) -> Self {
        // Initialize the origin during construction, never lazily under StateInner.
        let _ = *STATE_LOCK_ORIGIN;
        Self {
            instance: instance.to_owned(),
            fallback_epoch: next_state_lock_epoch(),
            active_id: 0.into(),
            active_epoch: 0.into(),
            active_site: std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()),
            active_requested: 0.into(),
            active_acquired: 0.into(),
            ring: Mutex::new(StateLockRing {
                slots: [None; STATE_LOCK_HISTORY],
                next: 0,
                count: 0,
            }),
            loss: 0.into(),
            evictions: 0.into(),
            changing: 0.into(),
            generation: 0.into(),
        }
    }

    fn acquired(
        &self,
        epoch: u64,
        site: &'static std::panic::Location<'static>,
        requested: std::time::Instant,
        acquired: std::time::Instant,
    ) -> StateLockSpan {
        use std::sync::atomic::Ordering::SeqCst;
        let span = StateLockSpan {
            id: STATE_LOCK_IDS.fetch_add(1, SeqCst),
            epoch,
            site,
            requested: state_lock_point(requested),
            acquired: state_lock_point(acquired),
            released: None,
        };
        // Invalidate before changing any field. Unique ids and SC operations
        // make a torn old/new snapshot detectable without waiting or a loop.
        self.changing.fetch_add(1, SeqCst);
        self.generation.fetch_add(1, SeqCst);
        self.active_id.store(u64::MAX, SeqCst);
        self.active_epoch.store(epoch, SeqCst);
        self.active_site
            .store(std::ptr::from_ref(site).cast_mut(), SeqCst);
        self.active_requested.store(span.requested, SeqCst);
        self.active_acquired.store(span.acquired, SeqCst);
        self.active_id.store(span.id, SeqCst);
        self.generation.fetch_add(1, SeqCst);
        self.changing.fetch_sub(1, SeqCst);
        span
    }

    fn active(&self) -> Result<Option<StateLockSpan>, ()> {
        use std::sync::atomic::Ordering::SeqCst;
        let id = self.active_id.load(SeqCst);
        if id == 0 {
            return Ok(None);
        }
        if id == u64::MAX {
            return Err(());
        }
        let epoch = self.active_epoch.load(SeqCst);
        let site = self.active_site.load(SeqCst);
        let requested = self.active_requested.load(SeqCst);
        let acquired = self.active_acquired.load(SeqCst);
        if site.is_null() || self.active_id.load(SeqCst) != id {
            return Err(());
        }
        // Only &'static Location pointers enter this atomic; their storage
        // never moves or expires. The double-id check protects field coherence.
        let site = unsafe { &*site };
        Ok(Some(StateLockSpan {
            id,
            epoch,
            site,
            requested,
            acquired,
            released: None,
        }))
    }

    fn begin_release(&self) {
        use std::sync::atomic::Ordering::SeqCst;
        self.changing.fetch_add(1, SeqCst);
        self.generation.fetch_add(1, SeqCst);
    }

    fn finish_release(&self, span: StateLockSpan) {
        use std::sync::atomic::Ordering::SeqCst;
        match self.ring.try_lock() {
            Ok(mut ring) => {
                let next = ring.next;
                if ring.count == STATE_LOCK_HISTORY {
                    self.evictions.fetch_add(1, SeqCst);
                }
                ring.slots[next] = Some(span);
                ring.next = (next + 1) % STATE_LOCK_HISTORY;
                ring.count = (ring.count + 1).min(STATE_LOCK_HISTORY);
            }
            Err(_) => {
                self.loss.fetch_add(1, SeqCst);
            }
        }
        // A predecessor may finish this callback after the successor acquired.
        // It must never clear that successor's active slot.
        let _ = self.active_id.compare_exchange(span.id, 0, SeqCst, SeqCst);
        self.generation.fetch_add(1, SeqCst);
        self.changing.fetch_sub(1, SeqCst);
    }

    #[cfg(test)]
    fn released(&self, span: StateLockSpan) {
        self.begin_release();
        self.finish_release(span);
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StateLockSite {
    file: String,
    line: u32,
    column: u32,
}

impl StateLockSite {
    fn from_location(site: &'static std::panic::Location<'static>) -> Option<Self> {
        let file = site.file().replace('\\', "/");
        if file.len() > 96
            || !file.starts_with("src/")
            || !file.ends_with(".rs")
            || file.split('/').any(|step| matches!(step, "" | "." | ".."))
            || !file
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b))
        {
            return None;
        }
        Some(Self {
            file,
            line: site.line(),
            column: site.column(),
        })
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StateLockWait {
    hold_id: String,
    site: StateLockSite,
    requested_micros: u64,
    acquired_micros: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StateLockHolder {
    hold_id: String,
    site: StateLockSite,
    acquired_micros: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    released_micros: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    age_micros: Option<u64>,
    state: &'static str,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StateLockBlock {
    contract: &'static str,
    version: u32,
    instance_id: String,
    mutex_epoch: String,
    loss_count: u64,
    eviction_count: u64,
    truncation_count: u64,
    partial: bool,
    unavailable: bool,
    note: &'static str,
    waits: Vec<StateLockWait>,
    holders: Vec<StateLockHolder>,
}

fn state_lock_overlap(holder: StateLockSpan, wait: StateLockSpan, now: u64) -> bool {
    holder.epoch == wait.epoch
        && holder.id != wait.id
        && holder.acquired < wait.acquired
        && holder.released.unwrap_or(now) > wait.requested
}

impl StateLockBlock {
    fn empty(instance: &str) -> Self {
        Self {
            contract: "termal-state-lock-observation",
            version: 1,
            instance_id: test_run_truncate(instance, 64),
            mutex_epoch: "0".to_owned(),
            loss_count: 0,
            eviction_count: 0,
            truncation_count: 0,
            partial: false,
            unavailable: false,
            note: "requested wait includes wall scheduling",
            waits: Vec::new(),
            holders: Vec::new(),
        }
    }

    fn collect(
        observer: &StateLockObserver,
        waits: &[Option<StateLockSpan>; 8],
        loss: u64,
    ) -> Self {
        Self::collect_at(
            observer,
            waits,
            loss,
            state_lock_point(std::time::Instant::now()),
        )
    }

    fn collect_at(
        observer: &StateLockObserver,
        waits: &[Option<StateLockSpan>; 8],
        loss: u64,
        now: u64,
    ) -> Self {
        use std::sync::atomic::Ordering::SeqCst;
        let mut block = Self::empty(&observer.instance);
        let epoch = waits
            .iter()
            .flatten()
            .last()
            .map_or(observer.fallback_epoch, |span| span.epoch);
        block.mutex_epoch = epoch.to_string();
        // All observer inputs, including counters, belong to this validated
        // snapshot. A change during any part of the copy makes it UNKNOWN.
        let generation = observer.generation.load(SeqCst);
        if observer.changing.load(SeqCst) != 0 {
            block.unavailable = true;
        }
        block.loss_count = observer.loss.load(SeqCst).saturating_add(loss);
        block.eviction_count = observer.evictions.load(SeqCst);
        #[cfg(test)]
        TEST_STATE_LOCK_COUNTERS_SAMPLED.with(|slot| {
            let hook = slot.borrow_mut().take();
            if let Some(hook) = hook {
                hook();
            }
        });
        let active = match observer.active() {
            Ok(active) => active,
            Err(_) => {
                block.unavailable = true;
                None
            }
        };
        // Copy a fixed-size array under try-lock, then release it before any
        // scan, JSON work, allocation, or source-path formatting.
        let copied = observer.ring.try_lock().map(|ring| ring.slots);
        let slots = match copied {
            Ok(slots) => slots,
            Err(_) => {
                block.unavailable = true;
                block.loss_count = block.loss_count.saturating_add(1);
                [None; STATE_LOCK_HISTORY]
            }
        };
        if observer.generation.load(SeqCst) != generation || observer.changing.load(SeqCst) != 0 {
            block.unavailable = true;
        }
        if block.unavailable {
            block.loss_count = block.loss_count.saturating_add(1);
        }
        for wait in waits.iter().flatten() {
            if wait.epoch != epoch {
                block.truncation_count += 1;
                continue;
            }
            if let Some(site) = StateLockSite::from_location(wait.site) {
                block.waits.push(StateLockWait {
                    hold_id: wait.id.to_string(),
                    site,
                    requested_micros: wait.requested,
                    acquired_micros: wait.acquired,
                });
            } else {
                block.truncation_count += 1;
            }
        }
        // This bounded candidate list is constructed and ranked off both
        // locks. Ring slot order must not decide which blocker is retained.
        let mut candidates: Vec<(StateLockSpan, (bool, u64, u64, u64))> =
            Vec::with_capacity(STATE_LOCK_HISTORY + 1);
        for holder in slots.iter().flatten().copied().chain(active) {
            if holder.epoch != epoch {
                if waits.iter().flatten().any(|wait| {
                    wait.epoch == epoch
                        && holder.id != wait.id
                        && holder.acquired < wait.acquired
                        && holder.released.unwrap_or(now) > wait.requested
                }) {
                    // Epochs are never joined, but an overlapping omitted
                    // domain must not look like fully observed idle time.
                    block.truncation_count += 1;
                }
                continue;
            }
            if !waits
                .iter()
                .flatten()
                .any(|wait| state_lock_overlap(holder, *wait, now))
            {
                continue;
            }
            if candidates.iter().any(|(row, _)| row.id == holder.id) {
                continue;
            }
            let end = holder.released.unwrap_or(now);
            let overlap = waits
                .iter()
                .flatten()
                .filter(|wait| wait.epoch == epoch && wait.id != holder.id)
                .map(|wait| {
                    end.min(wait.acquired)
                        .saturating_sub(holder.acquired.max(wait.requested))
                })
                .max()
                .unwrap_or(0);
            let priority = (
                holder.released.is_none(),
                overlap,
                end.saturating_sub(holder.acquired),
                holder.id,
            );
            candidates.push((holder, priority));
        }
        candidates.sort_unstable_by(|left, right| right.1.cmp(&left.1));
        block.truncation_count += candidates.len().saturating_sub(4) as u64;
        for (holder, _) in candidates.into_iter().take(4) {
            if let Some(site) = StateLockSite::from_location(holder.site) {
                block.holders.push(StateLockHolder {
                    hold_id: holder.id.to_string(),
                    site,
                    acquired_micros: holder.acquired,
                    released_micros: holder.released,
                    age_micros: holder
                        .released
                        .is_none()
                        .then(|| now.saturating_sub(holder.acquired)),
                    state: if holder.released.is_some() {
                        "released"
                    } else {
                        "active"
                    },
                });
            } else {
                block.truncation_count += 1;
            }
        }
        block.bound();
        block
    }

    fn bound(&mut self) {
        self.partial |= self.unavailable
            || self.loss_count > 0
            || self.eviction_count > 0
            || self.truncation_count > 0;
        // At most twelve rows enter this off-lock serialization loop.
        while serde_json::to_vec(self).map_or(true, |bytes| bytes.len() > 2048) {
            if self.holders.pop().is_none() && self.waits.pop().is_none() {
                break;
            }
            self.truncation_count = self.truncation_count.saturating_add(1);
            self.partial = true;
        }
    }
}
