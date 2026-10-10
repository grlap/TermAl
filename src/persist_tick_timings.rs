/*
Per-tick phase timings of the background persist writer.
Owns the timing record of one writer tick (collect, serialize, connection
open and pre-write checks, writer-ticket wait, SQL statements, commit
including any WAL checkpoint SQLite runs inside it, post-commit checks and
fence acknowledgement; a failed phase records its time too), the one
bounded log line a slow tick writes, and the summary a fence deadline
quotes. Does not own the writer loop (app_boot.rs), the SQL (persist.rs) or
fence resolution (persist_fence.rs). New file.
*/

/// A tick at least this slow writes one log line; faster ticks write none.
const PERSIST_SLOW_TICK_LOG_THRESHOLD: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PersistTickTimings {
    collect: Duration,
    serialize: Duration,
    /// Opening the cached connection (a reopen after a failed tick reruns
    /// schema setup) and the pre-write redirection check.
    open: Duration,
    ticket_wait: Duration,
    statements: Duration,
    /// COMMIT, which includes a WAL checkpoint when SQLite runs one there.
    commit: Duration,
    post_commit: Duration,
    acknowledge: Duration,
}

impl PersistTickTimings {
    fn total(&self) -> Duration {
        self.collect
            + self.serialize
            + self.open
            + self.ticket_wait
            + self.statements
            + self.commit
            + self.post_commit
            + self.acknowledge
    }

    fn summary(&self) -> String {
        format!(
            "collect {} ms, serialize {} ms, open {} ms, writer ticket {} ms, \
             statements {} ms, commit {} ms, post-commit {} ms, acknowledge {} ms",
            self.collect.as_millis(),
            self.serialize.as_millis(),
            self.open.as_millis(),
            self.ticket_wait.as_millis(),
            self.statements.as_millis(),
            self.commit.as_millis(),
            self.post_commit.as_millis(),
            self.acknowledge.as_millis(),
        )
    }

    fn log_if_slow(&self) {
        let total = self.total();
        if total >= PERSIST_SLOW_TICK_LOG_THRESHOLD {
            eprintln!(
                "[termal] slow persist tick ({} ms): {}",
                total.as_millis(),
                self.summary()
            );
        }
    }
}
