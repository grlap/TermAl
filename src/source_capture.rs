// Shared ownership and capacity for read-only source capture workers.
// Source identity itself is supplied by the configured Engram owner.

/// A definitive refusal retained by historical capture paths and Git reads.
#[derive(Clone, Copy, Debug)]
enum SourceBasisRefusal {
    ConfiguredGitFilters,
    UnsupportedGitlinks,
}

impl std::fmt::Display for SourceBasisRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ConfiguredGitFilters => "review verification does not run configured Git filters",
            Self::UnsupportedGitlinks => {
                "a content revision does not cover submodules (Git gitlinks are unsupported)"
            }
        })
    }
}

impl std::error::Error for SourceBasisRefusal {}

/// Runs `capture` on its own thread and waits at most `budget` for it. A file
/// read cannot be interrupted, so a capture that overruns stays owned by the
/// host lifetime until shutdown joins it (or retains pending cleanup). `live`
/// counts healthy and abandoned workers; no thread starts while at `limit`.
/// Failure, timeout, panic and capacity exhaustion all leave no capture.
fn bounded_source_capture<T: Send + 'static>(
    live: &Arc<std::sync::atomic::AtomicUsize>,
    lifetime: &Arc<CaptureLifetime>,
    limit: usize,
    budget: Duration,
    capture: impl FnOnce() -> Option<T> + Send + 'static,
) -> Option<T> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Release(Arc<AtomicUsize>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    if live
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
            (count < limit).then_some(count + 1)
        })
        .is_err()
    {
        eprintln!("engram> source not taken: {limit} captures still reading");
        return None;
    }
    let release = Release(live.clone());
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let spawned = lifetime.spawn("termal-source-capture", move || {
        let _release = release;
        let _ = sender.send(capture());
    });
    if let Err(error) = spawned {
        eprintln!("engram> no thread for a source capture: {error}");
        return None;
    }
    receiver.recv_timeout(budget).ok().flatten()
}
