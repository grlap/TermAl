//! Distinguish prepared transcript payload from committed SQLite row changes.
use super::*;

struct Fixture {
    cache: SqlitePersistConnectionCache,
    state: AppState,
    id: String,
    watermark: u64,
}

impl Fixture {
    fn new(count: usize) -> Self {
        let state = test_app_state();
        let id = test_session_id(&state, Agent::Codex);
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&id).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            for position in 0..count {
                push_message_on_record(record, Self::message(position));
            }
        }
        let delta = collect_persist_delta_from_shared_state(&state.inner, 0);
        let mut cache = SqlitePersistConnectionCache::new();
        persist_delta_via_cache(&mut cache, state.persistence_path.as_path(), &delta).unwrap();
        let watermark = delta.watermark;
        cache
            .connection
            .as_ref()
            .unwrap()
            .execute_batch(
                "CREATE TEMP TABLE message_changes(kind TEXT NOT NULL, bytes INTEGER NOT NULL);
             CREATE TEMP TRIGGER count_message_insert AFTER INSERT ON main.messages BEGIN
               INSERT INTO message_changes VALUES('insert',length(CAST(new.value_json AS BLOB)));
             END;
             CREATE TEMP TRIGGER count_message_update AFTER UPDATE ON main.messages BEGIN
               INSERT INTO message_changes VALUES('update',length(CAST(new.value_json AS BLOB)));
             END;
             CREATE TEMP TRIGGER count_message_delete AFTER DELETE ON main.messages BEGIN
               INSERT INTO message_changes VALUES('delete',length(CAST(old.value_json AS BLOB)));
             END;",
            )
            .unwrap();
        Self {
            state,
            id,
            cache,
            watermark,
        }
    }

    fn message(position: usize) -> Message {
        Message::Text {
            id: format!("transcript-{position}"),
            timestamp: "2026-01-01T00:00:00Z".into(),
            author: Author::Assistant,
            text: format!("{position}:{}", "stable-body-".repeat(1024)),
            expanded_text: None,
            source: None,
            attachments: vec![],
        }
    }

    fn change(&self, mutate: impl FnOnce(&mut SessionRecord)) {
        let mut inner = self.state.inner.lock().unwrap();
        let index = inner.find_session_index(&self.id).unwrap();
        mutate(inner.session_mut_by_index(index).unwrap());
    }

    fn persist_counts(&mut self) -> PersistObservedCounts {
        let observer = Arc::new(PersistObserver::default());
        let scope = observer.begin_at(std::time::Instant::now());
        let delta = collect_persist_delta_from_shared_state(&self.state.inner, self.watermark);
        persist_observe_delta(&delta);
        let mut timings = PersistTickTimings::default();
        persist_delta_via_cache_timed(
            &mut self.cache,
            self.state.persistence_path.as_path(),
            &delta,
            &mut timings,
        )
        .unwrap();
        self.watermark = delta.watermark;
        scope.finish(true, &timings);
        observer.snapshot_at(std::time::Instant::now()).recent[0]
            .counts
            .clone()
    }

    fn persist(&mut self) -> (PersistObservedCounts, usize, usize) {
        let counts = self.persist_counts();
        let (rows, bytes) = self
            .cache
            .connection
            .as_ref()
            .unwrap()
            .query_row(
                "SELECT count(*),coalesce(sum(bytes),0) FROM message_changes",
                [],
                |row| Ok((row.get::<_, usize>(0)?, row.get::<_, usize>(1)?)),
            )
            .unwrap();
        (counts, rows, bytes)
    }

    fn ordered_rows(&self) -> Vec<(String, String)> {
        let connection = self.cache.connection.as_ref().unwrap();
        let mut statement = connection
            .prepare(
                "SELECT message_id,value_json FROM messages WHERE session_id=?1 ORDER BY position",
            )
            .unwrap();
        statement
            .query_map([&self.id], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }

    fn clear_audit(&self) {
        self.cache
            .connection
            .as_ref()
            .unwrap()
            .execute("DELETE FROM message_changes", [])
            .unwrap();
    }

    fn overview_bytes(&self) -> Vec<u8> {
        self.cache
            .connection
            .as_ref()
            .unwrap()
            .query_row(
                "SELECT value_blob FROM session_overviews WHERE session_id=?1",
                [&self.id],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn assert_reload(&self, expected: &[(String, String)]) {
        let loaded = load_state(self.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        let record = &loaded.sessions[loaded.find_session_index(&self.id).unwrap()];
        assert_eq!(record.session.message_count as usize, expected.len());
        assert_eq!(
            record.message_start_index + record.session.messages.len(),
            expected.len()
        );
        for (message, (id, json)) in record
            .session
            .messages
            .iter()
            .zip(&expected[record.message_start_index..])
        {
            assert_eq!(message.id(), id);
            assert_eq!(
                serde_json::to_value(message).unwrap(),
                serde_json::from_str::<Value>(json).unwrap()
            );
        }
    }
}

#[test]
fn unchanged_tick_has_no_message_payload_or_sql_changes() {
    let mut fixture = Fixture::new(128);
    let before = fixture.ordered_rows();
    let (counts, rows, bytes) = fixture.persist();
    assert_eq!((rows, bytes), (0, 0));
    assert_eq!((counts.retained_messages, counts.message_bytes), (0, 0));
    assert_eq!(fixture.ordered_rows(), before);
}

#[test]
fn metadata_only_tick_prepares_no_unchanged_message_payload() {
    for retained in [16, 128] {
        let mut fixture = Fixture::new(retained);
        let before = fixture.ordered_rows();
        fixture.change(|record| record.session.preview = "metadata-only".into());
        let (counts, rows, bytes) = fixture.persist();
        assert_eq!(
            (rows, bytes),
            (0, 0),
            "identical SQLite rows already stay unchanged"
        );
        assert_eq!(fixture.ordered_rows(), before);
        assert_eq!(
            (counts.retained_messages, counts.message_bytes),
            (0, 0),
            "metadata-only change prepared {} messages / {} bytes for {retained} retained messages; actual changed rows={rows}",
            counts.retained_messages,
            counts.message_bytes
        );
    }
}

#[test]
fn one_streaming_edit_prepares_only_the_changed_message() {
    for retained in [16, 128] {
        let mut fixture = Fixture::new(retained);
        let before = fixture.ordered_rows();
        fixture.change(|record| {
            let Message::Text { text, .. } = &mut record.session.messages[0] else {
                panic!()
            };
            text.push_str("streamed suffix");
        });
        let (counts, rows, bytes) = fixture.persist();
        assert_eq!(rows, 1);
        let after = fixture.ordered_rows();
        assert_eq!(&after[1..], &before[1..]);
        assert!(after[0].1.contains("streamed suffix"));
        assert_eq!(
            (counts.retained_messages, counts.message_bytes),
            (1, bytes),
            "one changed row prepared {} messages / {} bytes for {retained} retained messages; actual changed bytes={bytes}",
            counts.retained_messages,
            counts.message_bytes
        );
    }
}

#[test]
fn one_append_prepares_only_the_new_message_and_preserves_order() {
    for retained in [16, 128] {
        let mut fixture = Fixture::new(retained);
        let before = fixture.ordered_rows();
        fixture.change(|record| {
            push_message_on_record(record, Fixture::message(retained));
        });
        let (counts, rows, bytes) = fixture.persist();
        assert_eq!(rows, 1);
        let after = fixture.ordered_rows();
        assert_eq!(&after[..retained], before.as_slice());
        assert_eq!(after[retained].0, format!("transcript-{retained}"));
        assert_eq!(
            (counts.retained_messages, counts.message_bytes),
            (1, bytes),
            "one append prepared {} messages / {} bytes for {retained} retained messages; actual inserted bytes={bytes}",
            counts.retained_messages,
            counts.message_bytes
        );
    }
}

#[test]
fn cold_prefix_eviction_does_not_delete_or_resubmit_durable_bodies() {
    let mut fixture = Fixture::new(128);
    let before = fixture.ordered_rows();
    fixture.change(|record| trim_retained_session_messages(record, 8));
    let (counts, rows, _) = fixture.persist();
    assert_eq!((counts.retained_messages, rows), (0, 0));
    assert_eq!(fixture.ordered_rows(), before);
    fixture.change(|record| {
        let Message::Text { text, .. } = &mut record.session.messages[0] else {
            panic!()
        };
        text.push_str("cold-tail edit");
    });
    let (counts, rows, _) = fixture.persist();
    assert_eq!((counts.retained_messages, rows), (1, 1));
    let after = fixture.ordered_rows();
    assert_eq!(&after[..120], &before[..120]);
    assert_eq!(&after[121..], &before[121..]);
    fixture.assert_reload(&after);
}

#[test]
fn reorder_truncate_and_same_id_import_preserve_exact_reload_order() {
    let mut fixture = Fixture::new(128);
    let mut expected = fixture.ordered_rows();
    fixture.change(|record| record.session.messages.swap(0, 127));
    let (counts, rows, _) = fixture.persist();
    assert_eq!(
        (counts.retained_messages, rows),
        (2, 4),
        "two identities released before their inserts"
    );
    expected.swap(0, 127);
    assert_eq!(fixture.ordered_rows(), expected);
    fixture.assert_reload(&expected);
    fixture.clear_audit();
    fixture.change(|record| {
        record.session.messages.truncate(3);
        sync_retained_transcript_metadata(record);
    });
    let (counts, rows, _) = fixture.persist();
    assert_eq!((counts.retained_messages, rows), (0, 125));
    expected.truncate(3);
    assert_eq!(fixture.ordered_rows(), expected);
    fixture.assert_reload(&expected);
    fixture.clear_audit();
    fixture.change(|record| {
        let mut imported = Fixture::message(1);
        let Message::Text { text, .. } = &mut imported else {
            panic!()
        };
        *text = "same-id imported replacement".into();
        replace_session_messages_on_record(record, vec![imported], None);
    });
    let (counts, _, _) = fixture.persist();
    assert_eq!(counts.retained_messages, 1);
    let after = fixture.ordered_rows();
    assert_eq!(after.len(), 1);
    assert!(after[0].1.contains("same-id imported replacement"));
    fixture.assert_reload(&after);
}

#[test]
fn failed_transaction_does_not_publish_a_baseline_and_reopen_retries_exact_content() {
    let mut fixture = Fixture::new(16);
    let before = fixture.ordered_rows();
    fixture.change(|record| {
        let Message::Text { text, .. } = &mut record.session.messages[0] else {
            panic!()
        };
        text.push_str("retry body");
    });
    let delta = collect_persist_delta_from_shared_state(&fixture.state.inner, fixture.watermark);
    fixture.cache.connection.as_ref().unwrap().execute_batch(
        "CREATE TEMP TRIGGER refuse_message BEFORE UPDATE ON main.messages BEGIN SELECT RAISE(ABORT,'injected precommit failure'); END;"
    ).unwrap();
    let error = persist_delta_via_cache(
        &mut fixture.cache,
        fixture.state.persistence_path.as_path(),
        &delta,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("injected precommit failure"));
    assert!(fixture.cache.connection.is_none());
    assert!(fixture.cache.transcripts.is_empty());
    fixture.assert_reload(&before);
    // The same captured delta is still authoritative; failed work did not
    // advance its watermark or its immutable-body predecessor.
    persist_delta_via_cache(
        &mut fixture.cache,
        fixture.state.persistence_path.as_path(),
        &delta,
    )
    .unwrap();
    let after = fixture.ordered_rows();
    assert_eq!(&after[1..], &before[1..]);
    assert!(after[0].1.contains("retry body"));
    fixture.assert_reload(&after);
    fixture.cache.invalidate();
    fixture.change(|record| record.session.preview = "after reopen".into());
    let delta = collect_persist_delta_from_shared_state(&fixture.state.inner, fixture.watermark);
    persist_delta_via_cache(
        &mut fixture.cache,
        fixture.state.persistence_path.as_path(),
        &delta,
    )
    .unwrap();
    assert_eq!(fixture.ordered_rows(), after);
}

#[test]
fn snapshot_shares_unchanged_bodies_and_serialization_releases_the_state_lock() {
    let mut fixture = Fixture::new(128);
    fixture.change(|record| record.session.preview = "capture before concurrent edit".into());
    let shared = fixture.state.inner.clone();
    let id = fixture.id.clone();
    PERSIST_MESSAGE_SERIALIZE_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move |snapshot| {
            let mut inner = shared
                .try_lock_now()
                .expect("serialization must run outside StateInner");
            let index = inner.find_session_index(&id).unwrap();
            assert!(
                (0..128).all(|n| snapshot.session.messages.shares_body(
                    n,
                    &inner.sessions[index].session.messages,
                    n
                )),
                "capturing a snapshot must not clone any unchanged retained body"
            );
            let record = inner.session_mut_by_index(index).unwrap();
            let Message::Text { text, .. } = &mut record.session.messages[7] else {
                panic!()
            };
            text.push_str("newer than captured watermark");
            assert!(
                !snapshot
                    .session
                    .messages
                    .shares_body(7, &record.session.messages, 7)
            );
            assert!(
                snapshot
                    .session
                    .messages
                    .shares_body(8, &record.session.messages, 8)
            );
            assert!(
                !serde_json::to_string(&snapshot.session.messages[7])
                    .unwrap()
                    .contains("newer than captured watermark")
            );
        }));
    });
    let (counts, rows, _) = fixture.persist();
    assert_eq!((counts.retained_messages, rows), (0, 0));
    let (counts, rows, _) = fixture.persist();
    assert_eq!(
        (counts.retained_messages, rows),
        (1, 1),
        "the newer edit must remain dirty after the older capture commits"
    );
    let after = fixture.ordered_rows();
    assert!(after[7].1.contains("newer than captured watermark"));
    fixture.assert_reload(&after);
}

#[test]
fn another_connection_commit_invalidates_the_transcript_predecessor() {
    let mut fixture = Fixture::new(16);
    let before = fixture.ordered_rows();
    {
        let other = rusqlite::Connection::open(fixture.state.persistence_path.as_path()).unwrap();
        other
            .execute(
                "UPDATE messages SET value_json=?1 WHERE session_id=?2 AND position=0",
                rusqlite::params![
                    serde_json::to_string(&Fixture::message(999)).unwrap(),
                    fixture.id
                ],
            )
            .unwrap();
    }
    fixture.change(|record| record.session.preview = "external predecessor changed".into());
    let (counts, rows, _) = fixture.persist();
    assert_eq!(
        (counts.retained_messages, rows),
        (16, 1),
        "a different store predecessor requires complete captured coverage"
    );
    assert_eq!(fixture.ordered_rows(), before);
    fixture.assert_reload(&before);
}

#[test]
fn postcommit_external_writer_cannot_be_adopted_as_our_baseline() {
    let mut fixture = Fixture::new(16);
    let before = fixture.ordered_rows();
    let path = fixture.state.persistence_path.as_ref().clone();
    let id = fixture.id.clone();
    PERSIST_MESSAGE_COMMIT_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            // A separate connection stands in for an external writer, which
            // does not participate in this process's writer admission lock.
            let other = rusqlite::Connection::open(path).unwrap();
            other
                .execute(
                    "UPDATE messages SET value_json=?1 WHERE session_id=?2 AND position=0",
                    rusqlite::params![serde_json::to_string(&Fixture::message(999)).unwrap(), id],
                )
                .unwrap();
        }));
    });
    fixture.change(|record| record.session.preview = "our metadata commit".into());
    let (counts, rows, _) = fixture.persist();
    assert_eq!((counts.retained_messages, rows), (0, 0));
    assert_ne!(
        fixture.ordered_rows(),
        before,
        "the external writer really committed after ours"
    );
    fixture.change(|record| record.session.preview = "next metadata commit".into());
    let (counts, rows, _) = fixture.persist();
    assert_eq!(
        (counts.retained_messages, rows),
        (16, 1),
        "an external postcommit version must never certify our older body snapshot"
    );
    assert_eq!(fixture.ordered_rows(), before);
    fixture.assert_reload(&before);
}

#[test]
fn external_commit_during_preparation_refuses_sparse_write_and_retries_full_capture() {
    let mut fixture = Fixture::new(16);
    let before = fixture.ordered_rows();
    fixture.change(|record| record.session.preview = "prepared against old store".into());
    let delta = collect_persist_delta_from_shared_state(&fixture.state.inner, fixture.watermark);
    let path = fixture.state.persistence_path.as_ref().clone();
    let id = fixture.id.clone();
    PERSIST_MESSAGE_SERIALIZE_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move |_| {
            let other = rusqlite::Connection::open(path).unwrap();
            other
                .execute(
                    "UPDATE messages SET value_json=?1 WHERE session_id=?2 AND position=0",
                    rusqlite::params![serde_json::to_string(&Fixture::message(999)).unwrap(), id],
                )
                .unwrap();
        }));
    });
    let error = persist_delta_via_cache(
        &mut fixture.cache,
        fixture.state.persistence_path.as_path(),
        &delta,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("transcript predecessor changed"));
    assert!(fixture.cache.connection.is_none());
    assert!(fixture.cache.transcripts.is_empty());
    persist_delta_via_cache(
        &mut fixture.cache,
        fixture.state.persistence_path.as_path(),
        &delta,
    )
    .unwrap();
    assert_eq!(fixture.ordered_rows(), before);
    fixture.assert_reload(&before);
}

#[test]
fn no_op_recovery_and_stop_scans_do_not_detach_unchanged_bodies() {
    for retained in [16, 128] {
        let mut fixture = Fixture::new(retained);
        let before = fixture.ordered_rows();
        fixture.change(|record| {
            assert_eq!(
                expire_pending_interaction_messages(&mut record.session.messages),
                0
            );
            assert!(cancel_pending_interaction_messages(&mut record.session.messages).is_empty());
            fail_running_command_messages(&mut record.session.messages);
        });
        let (counts, rows, bytes) = fixture.persist();
        assert_eq!(
            (counts.retained_messages, counts.message_bytes, rows, bytes),
            (0, 0, 0, 0)
        );
        assert_eq!(fixture.ordered_rows(), before);
    }
}

#[test]
fn command_recovery_detaches_only_the_running_message() {
    let mut fixture = Fixture::new(128);
    fixture.change(|record| {
        record.session.messages[7] = Message::Command {
            id: "transcript-7".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            author: Author::Assistant,
            command: "local fixture".into(),
            command_language: None,
            output: String::new(),
            output_language: None,
            status: CommandStatus::Running,
        };
    });
    assert_eq!(fixture.persist().0.retained_messages, 1);
    let before = fixture.ordered_rows();
    fixture.clear_audit();
    fixture.change(|record| fail_running_command_messages(&mut record.session.messages));
    let (counts, rows, bytes) = fixture.persist();
    assert_eq!((counts.retained_messages, rows), (1, 1));
    assert_eq!(counts.message_bytes, bytes);
    let after = fixture.ordered_rows();
    assert_eq!(&after[..7], &before[..7]);
    assert_eq!(&after[8..], &before[8..]);
    fixture.clear_audit();
    fixture.change(|record| fail_running_command_messages(&mut record.session.messages));
    let (counts, rows, _) = fixture.persist();
    assert_eq!(
        (counts.retained_messages, rows),
        (0, 0),
        "already settled commands remain shared"
    );
    fixture.assert_reload(&after);
}

#[test]
fn persisted_causal_projection_includes_unpublished_evidence_without_changing_live_body() {
    let mut fixture = Fixture::new(16);
    let mut card: EngramControlCard = serde_json::from_value(json!({
        "schemaVersion":1, "stage":"checkpoint", "assurance":"fixture",
        "decision":"refuse", "dispatch":"withheld", "latencyMs":{"total":0}, "failMode":"enforced"
    }))
    .unwrap();
    let mut cause = EngramCausalFailure::local(
        &EngramTransportError::local_state("persist exact causal evidence"),
        "fixture",
        "local fixture",
    );
    cause.publication_pending = true;
    card.causal_failure = Some(cause);
    fixture.change(|record| {
        push_message_on_record(
            record,
            Message::EngramControl {
                id: "causal-card".into(),
                timestamp: "2026-01-01T00:00:00Z".into(),
                author: Author::Assistant,
                card,
            },
        );
    });
    let (counts, rows, _) = fixture.persist();
    assert_eq!((counts.retained_messages, rows), (1, 1));
    let after = fixture.ordered_rows();
    let persisted: Value = serde_json::from_str(&after[16].1).unwrap();
    assert!(persisted["causalFailure"].is_object());
    let captured = {
        let inner = fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.id).unwrap()];
        let Message::EngramControl { card, .. } = &record.session.messages[16] else {
            panic!()
        };
        assert!(card.causal_failure.as_ref().unwrap().publication_pending);
        assert!(
            serde_json::to_value(&record.session.messages[16])
                .unwrap()
                .get("causalFailure")
                .is_none()
        );
        PersistedSessionRecord::from_record(record)
    };
    assert!(
        serde_json::to_value(captured).unwrap()["session"]["messages"][16]["causalFailure"]
            .is_object()
    );
    fixture.clear_audit();
    fixture.change(|record| record.session.preview = "unrelated to pending publication".into());
    let (counts, rows, _) = fixture.persist();
    assert_eq!((counts.retained_messages, rows), (0, 0));
    fixture.assert_reload(&after);
}

#[test]
fn durable_baseline_does_not_keep_evicted_message_bodies_alive() {
    let mut fixture = Fixture::new(128);
    let witnesses = {
        let inner = fixture.state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&fixture.id).unwrap()]
            .session
            .messages
            .identities()
    };
    trim_persisted_session_tails_from_shared_state(
        &fixture.state.inner,
        fixture.watermark,
        std::slice::from_ref(&fixture.id),
    );
    {
        let inner = fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.id).unwrap()];
        assert_eq!(record.message_start_index, 64);
        assert_eq!(record.session.messages.len(), 64);
    }
    assert!(fixture.cache.connection.is_some());
    assert!(fixture.cache.transcripts.contains_key(&fixture.id));
    assert!(
        witnesses[0].upgrade().is_none(),
        "the committed baseline must not retain an evicted body"
    );
    assert!(
        witnesses[127].upgrade().is_some(),
        "the live suffix must remain resident"
    );
    fixture
        .cache
        .release_evicted_transcript_prefixes(std::slice::from_ref(&fixture.id));
    let baseline = &fixture.cache.transcripts[&fixture.id];
    assert_eq!(baseline.start, 64);
    assert_eq!(baseline.identities.len(), 64);
    let before = fixture.ordered_rows();
    fixture.change(|record| record.session.preview = "after eviction".into());
    let (counts, rows, _) = fixture.persist();
    assert_eq!((counts.retained_messages, rows), (0, 0));
    assert_eq!(fixture.ordered_rows(), before);
    fixture.assert_reload(&before);
}

#[test]
fn postcommit_integrity_failure_discards_baseline_before_full_retry() {
    let mut fixture = Fixture::new(16);
    fixture.change(|record| {
        let Message::Text { text, .. } = &mut record.session.messages[0] else {
            panic!()
        };
        text.push_str("committed but not verified");
    });
    let delta = collect_persist_delta_from_shared_state(&fixture.state.inner, fixture.watermark);
    METADATA_COMMIT_INTEGRITY_FAULT.with(|fault| fault.set(true));
    let error = persist_delta_via_cache(
        &mut fixture.cache,
        fixture.state.persistence_path.as_path(),
        &delta,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("injected metadata post-commit integrity failure"));
    assert!(fixture.cache.connection.is_none());
    assert!(fixture.cache.transcripts.is_empty());
    assert!(fixture.cache.transcript_data_version.is_none());
    // COMMIT happened, but neither its predecessor nor its watermark was
    // accepted. Retrying must prepare all bodies even though SQL is identical.
    let loaded = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&fixture.id).unwrap()];
    assert!(
        serde_json::to_string(&record.session.messages[0])
            .unwrap()
            .contains("committed but not verified")
    );
    let counts = fixture.persist_counts();
    assert_eq!(counts.retained_messages, 16);
    let after = fixture.ordered_rows();
    assert_eq!(after.len(), 16);
    assert!(after[0].1.contains("committed but not verified"));
    fixture.assert_reload(&after);
}

#[test]
fn removal_and_hiding_drop_baselines_before_reappearance() {
    for hidden in [false, true] {
        let mut fixture = Fixture::new(16);
        let before = fixture.ordered_rows();
        let removed = if hidden {
            fixture.change(|record| record.hidden = true);
            None
        } else {
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.id).unwrap();
            inner.remove_session_at(index)
        };
        fixture.persist();
        assert!(fixture.ordered_rows().is_empty());
        assert!(!fixture.cache.transcripts.contains_key(&fixture.id));
        fixture.clear_audit();
        if hidden {
            fixture.change(|record| record.hidden = false);
        } else {
            fixture
                .state
                .inner
                .lock()
                .unwrap()
                .push_session(removed.unwrap());
        }
        let (counts, rows, _) = fixture.persist();
        assert_eq!((counts.retained_messages, rows), (16, 16));
        assert_eq!(fixture.ordered_rows(), before);
        fixture.assert_reload(&before);
    }
}

#[test]
fn sparse_partial_extent_preserves_unobserved_suffix_and_overview() {
    let mut fixture = Fixture::new(16);
    let before = fixture.ordered_rows();
    let mut overview = fixture.overview_bytes();
    fixture.change(|record| {
        record.session.messages = record.session.messages.window(4..8);
        record.message_start_index = 4;
        record.session.messages_loaded = false;
        record.session.message_count = 16;
        record.session.messages[1] = Message::Command {
            id: "transcript-5".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            author: Author::Assistant,
            command: "partial edit".into(),
            command_language: None,
            output: "only the observed position changes".into(),
            output_language: None,
            status: CommandStatus::Success,
        };
    });
    overview[5] = encode_conversation_overview_message(ConversationOverviewKind::Command, false);
    let (counts, rows, _) = fixture.persist();
    assert_eq!((counts.retained_messages, rows), (1, 1));
    let after = fixture.ordered_rows();
    assert_eq!(&after[..5], &before[..5]);
    assert_eq!(&after[6..], &before[6..]);
    assert_eq!(after.len(), 16);
    assert!(after[5].1.contains("partial edit"));
    assert_eq!(fixture.overview_bytes(), overview);
    fixture.assert_reload(&after);
    fixture.clear_audit();
    fixture.change(|record| record.session.preview = "partial metadata only".into());
    let (counts, rows, _) = fixture.persist();
    assert_eq!((counts.retained_messages, rows), (0, 0));
    assert_eq!(fixture.ordered_rows(), after);
    assert_eq!(fixture.overview_bytes(), overview);
}

#[test]
fn invalid_session_preserves_its_previous_body_baseline_until_repaired() {
    let mut fixture = Fixture::new(16);
    let before = fixture.ordered_rows();
    fixture.change(|record| {
        record.remote_id = Some("invalid-without-remote-session".into());
        let Message::Text { text, .. } = &mut record.session.messages[0] else {
            panic!()
        };
        text.push_str("pending while invalid");
    });
    let (counts, rows, _) = fixture.persist();
    assert_eq!((counts.retained_messages, rows), (0, 0));
    assert_eq!(fixture.ordered_rows(), before);
    assert!(fixture.cache.transcripts.contains_key(&fixture.id));
    fixture.change(|record| record.remote_id = None);
    let (counts, rows, _) = fixture.persist();
    assert_eq!((counts.retained_messages, rows), (1, 1));
    let after = fixture.ordered_rows();
    assert!(after[0].1.contains("pending while invalid"));
    assert_eq!(&after[1..], &before[1..]);
    fixture.assert_reload(&after);
}

#[test]
fn prefix_hydration_resends_prefix_but_skips_shared_durable_tail() {
    let mut fixture = Fixture::new(128);
    let before = fixture.ordered_rows();
    fixture.change(|record| trim_retained_session_messages(record, 8));
    fixture.persist();
    assert_eq!(fixture.cache.transcripts[&fixture.id].start, 120);
    fixture.clear_audit();
    fixture.change(|record| {
        // Supply the cold prefix without replacing the shared resident tail.
        for position in (0..120).rev() {
            record
                .session
                .messages
                .insert(0, Fixture::message(position));
        }
        record.message_start_index = 0;
        sync_retained_transcript_metadata(record);
    });
    let (counts, rows, _) = fixture.persist();
    assert_eq!((counts.retained_messages, rows), (120, 0));
    assert_eq!(fixture.ordered_rows(), before);
    fixture.assert_reload(&before);
}
