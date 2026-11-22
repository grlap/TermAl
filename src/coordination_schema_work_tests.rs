//! Disposable-store work and phase measurements for coordination schema opening.
//! The production schema owner supplies opt-in, thread-local spans; timings are
//! diagnostics only. Regression assertions measure unnecessary work and data.

use super::*;
use std::cell::RefCell;
use std::time::{Duration, Instant};

#[derive(Debug)]
struct Sample {
    phase: &'static str,
    version: Option<String>,
    elapsed: Duration,
}

thread_local! {
    static SAMPLES: RefCell<Option<Vec<Sample>>> = const { RefCell::new(None) };
}

pub(crate) struct Span {
    phase: &'static str,
    version: Option<String>,
    started: Instant,
}

pub(crate) fn span(phase: &'static str, version: Option<&str>) -> Option<Span> {
    SAMPLES.with(|samples| {
        samples.borrow().as_ref()?;
        Some(Span {
            phase,
            version: version.map(str::to_owned),
            started: Instant::now(),
        })
    })
}

impl Drop for Span {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed();
        SAMPLES.with(|samples| {
            if let Some(samples) = samples.borrow_mut().as_mut() {
                samples.push(Sample {
                    phase: self.phase,
                    version: self.version.take(),
                    elapsed,
                });
            }
        });
    }
}

fn capture<T>(action: impl FnOnce() -> T) -> (T, Vec<Sample>) {
    struct Restore(Option<Vec<Sample>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SAMPLES.with(|samples| *samples.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(SAMPLES.with(|samples| samples.replace(Some(Vec::new()))));
    let result = action();
    let samples = SAMPLES.with(|samples| samples.borrow_mut().take().unwrap());
    (result, samples)
}

fn report(label: &str, samples: &[Sample]) {
    for sample in samples {
        eprintln!(
            "coordination_schema_work open={label} phase={} version={} elapsed_us={}",
            sample.phase,
            sample.version.as_deref().unwrap_or("-"),
            sample.elapsed.as_micros(),
        );
    }
    eprintln!(
        "coordination_schema_work open={label} canonical_builds={} historical_builds={}",
        samples
            .iter()
            .filter(|sample| sample.phase == "canonical")
            .count(),
        samples
            .iter()
            .filter(|sample| {
                sample.phase == "canonical"
                    && sample.version.as_deref() != Some(COORDINATION_SQLITE_SCHEMA_VERSION)
            })
            .count(),
    );
}

#[test]
fn coordination_schema_work_boot_reopen_phase_diagnostic() {
    let root = TestTempRoot::create("coord-schema-phases");
    let path = root.path().join("coordination.sqlite");
    drop(bootstrap_coordination_database(&path).unwrap());

    for restart in 0..2 {
        let (mailbox_connection, samples) = capture(|| {
            let mailbox_connection = bootstrap_coordination_database(&path).unwrap();
            let board = CoordinationBoardStore::open(&path).unwrap();
            drop(board);
            mailbox_connection
        });
        report(&format!("restart-{restart}"), &samples);
        validate_current_coordination_schema(&mailbox_connection).unwrap();
        assert!(
            samples
                .iter()
                .any(|sample| sample.phase == "writer.admission")
        );
        assert!(
            samples
                .iter()
                .any(|sample| sample.phase == "inspection.snapshot")
        );
        assert!(
            samples
                .iter()
                .any(|sample| sample.phase == "inspection.contents")
        );
    }
}

#[test]
fn coordination_schema_work_current_reopen_needs_no_historical_builds() {
    let root = TestTempRoot::create("coord-schema-current-work");
    let path = root.path().join("coordination.sqlite");
    let connection = bootstrap_coordination_database(&path).unwrap();
    connection
        .execute(
            "INSERT INTO mailboxes(id, participant_key, created_at, next_sequence)
         VALUES('preserved', 'participants', 'now', 17)",
            [],
        )
        .unwrap();
    connection.execute_batch("PRAGMA query_only = ON;").unwrap();
    let before = coordination_schema_objects(&connection).unwrap();
    let (_, samples) = capture(|| {
        ensure_sqlite_coordination_schema_for_path(&connection, &path).unwrap();
        let board = CoordinationBoardStore::open(&path).unwrap();
        drop(board);
    });
    report("current-reopen", &samples);
    assert_eq!(coordination_schema_objects(&connection).unwrap(), before);
    assert_eq!(
        connection
            .query_row(
                "SELECT next_sequence FROM mailboxes WHERE id = 'preserved'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        17
    );
    assert!(
        samples.iter().all(|sample| {
            sample.phase != "canonical"
                || sample.version.as_deref() == Some(COORDINATION_SQLITE_SCHEMA_VERSION)
        }),
        "reopening a validated current store must not build historical schemas: {samples:?}"
    );
}

#[test]
fn coordination_schema_work_current_layout_and_marker_must_both_match() {
    for mutation in [
        "CREATE TABLE unexpected(value TEXT);",
        "DROP INDEX mailbox_messages_topic_key_sender;",
        "DROP INDEX mailbox_messages_topic_key_sender;
         CREATE INDEX mailbox_messages_topic_key_sender ON mailbox_messages(topic);",
        "DELETE FROM meta WHERE key='coordination_schema_version';",
        "UPDATE meta SET value='1' WHERE key='coordination_schema_version';",
        "UPDATE meta SET value=X'ff' WHERE key='coordination_schema_version';",
    ] {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        ensure_sqlite_coordination_schema(&connection).unwrap();
        connection.execute_batch(mutation).unwrap();
        let schema = coordination_schema_objects(&connection).unwrap();
        let metadata = || {
            connection
                .prepare("SELECT key, value FROM meta ORDER BY key")
                .unwrap()
                .query_map([], |row| {
                    Ok((
                        row.get::<_, rusqlite::types::Value>(0)?,
                        row.get::<_, rusqlite::types::Value>(1)?,
                    ))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        let before = metadata();
        let changes = connection.total_changes();
        connection.execute_batch("PRAGMA query_only=ON;").unwrap();
        assert!(
            ensure_sqlite_coordination_schema(&connection).is_err(),
            "{mutation}"
        );
        assert!(
            connection.is_autocommit(),
            "refusal must release the snapshot: {mutation}"
        );
        assert_eq!(
            coordination_schema_objects(&connection).unwrap(),
            schema,
            "{mutation}"
        );
        assert_eq!(metadata(), before, "{mutation}");
        assert_eq!(connection.total_changes(), changes, "{mutation}");
    }
}

#[test]
fn coordination_schema_work_parallel_restarts_preserve_the_current_store() {
    let root = TestTempRoot::create("coord-schema-parallel");
    let path = root.path().join("coordination.sqlite");
    let connection = bootstrap_coordination_database(&path).unwrap();
    connection
        .execute(
            "INSERT INTO mailboxes(id, participant_key, created_at, next_sequence)
         VALUES('preserved', 'participants', 'now', 17)",
            [],
        )
        .unwrap();
    let before = coordination_schema_objects(&connection).unwrap();
    drop(connection);
    let ready = std::sync::Barrier::new(4);
    std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for _ in 0..4 {
            workers.push(scope.spawn(|| {
                ready.wait();
                let (_, samples) = capture(|| {
                    for _ in 0..3 {
                        let mailbox_connection = bootstrap_coordination_database(&path).unwrap();
                        let board = CoordinationBoardStore::open(&path).unwrap();
                        let sequence = mailbox_connection
                            .query_row(
                                "SELECT next_sequence FROM mailboxes WHERE id='preserved'",
                                [],
                                |row| row.get::<_, i64>(0),
                            )
                            .unwrap();
                        assert_eq!(sequence, 17);
                        drop(board);
                    }
                });
                let only_current = samples.iter().all(|sample| {
                    sample.phase != "canonical"
                        || sample.version.as_deref() == Some(COORDINATION_SQLITE_SCHEMA_VERSION)
                });
                assert!(
                    only_current,
                    "parallel reopens built historical schemas: {samples:?}"
                );
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
    });
    let connection = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(coordination_schema_objects(&connection).unwrap(), before);
    validate_current_coordination_schema(&connection).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT next_sequence FROM mailboxes WHERE id='preserved'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        17
    );
}
