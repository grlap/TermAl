//! General commits queue metadata publication without projecting every session.

use super::*;
use std::sync::atomic::AtomicUsize;

#[path = "initial_state_snapshot.rs"]
mod initial_state_snapshot;

#[path = "state_diff_broadcast.rs"]
mod state_diff_broadcast;

struct ProjectionObserver;

impl ProjectionObserver {
    fn install(projections: Arc<AtomicUsize>) -> Self {
        TEST_BEFORE_STATE_SNAPSHOT_PROJECT.with(|hook| {
            assert!(hook.borrow().is_none());
            *hook.borrow_mut() = Some(Box::new(move || {
                projections.fetch_add(1, Ordering::SeqCst);
            }));
        });
        Self
    }
}

impl Drop for ProjectionObserver {
    fn drop(&mut self) {
        TEST_BEFORE_STATE_SNAPSHOT_PROJECT.with(|hook| {
            hook.borrow_mut().take();
        });
    }
}

#[test]
fn general_commits_do_not_project_full_state_before_the_broadcaster_runs() {
    let mut state = test_app_state();
    // This is the production mailbox configuration. No consumer runs until
    // after we have observed the completed commits and reacquired StateInner.
    state.state_broadcast_mailbox = Some(Arc::new(StateBroadcastMailbox::default()));
    let initial_revision = state.inner.lock().unwrap().revision;
    let projections = Arc::new(AtomicUsize::new(0));
    let (committed, commits_finished) = mpsc::channel();
    let (read_snapshot, may_read_snapshot) = mpsc::channel();
    let producer = {
        let state = state.clone();
        let projections = projections.clone();
        std::thread::spawn(move || {
            let _observer = ProjectionObserver::install(projections.clone());
            for _ in 0..3 {
                let mut inner = state.inner.lock().unwrap();
                state.commit_locked(&mut inner).unwrap();
            }
            let before_explicit_read = projections.load(Ordering::SeqCst);
            committed.send(before_explicit_read).unwrap();
            may_read_snapshot
                .recv_timeout(phase_sync::DEADLOCK_GUARD)
                .expect("explicit snapshot phase was released");
            // A positive control: the same observer must see an actual full
            // projection when a reader explicitly requests current metadata.
            let snapshot = state.summary_snapshot();
            (snapshot.revision, projections.load(Ordering::SeqCst))
        })
    };
    let before_explicit_read = commits_finished
        .recv_timeout(phase_sync::DEADLOCK_GUARD)
        .expect("general commits finish without a broadcaster consumer");
    let committed_revision = state.inner.lock().unwrap().revision;
    // Release and join before judging the result, including the RED witness.
    read_snapshot.send(()).unwrap();
    let (read_revision, after_explicit_read) = producer.join().unwrap();
    assert_eq!(committed_revision, initial_revision + 3);
    assert_eq!(read_revision, committed_revision);
    assert_eq!(
        after_explicit_read,
        before_explicit_read + 1,
        "the observer must detect the explicit metadata projection"
    );
    assert_eq!(
        before_explicit_read, 0,
        "general commits built full snapshots while holding StateInner, before the broadcaster ran"
    );
}

fn cached_state() -> (AppState, String, Arc<StateBroadcastMailbox>) {
    let mut state = test_app_state();
    let session = test_session_id(&state, Agent::Codex);
    state
        .push_message(
            &session,
            Message::Text {
                id: "body".to_owned(),
                author: Author::Assistant,
                text: "start".to_owned(),
                timestamp: stamp_now(),
                attachments: Vec::new(),
                expanded_text: None,
                source: None,
            },
        )
        .unwrap();
    let (stream_events, _) = broadcast::channel(1024);
    state.state_broadcast_senders.stream_events = stream_events;
    let mailbox = Arc::new(StateBroadcastMailbox::default());
    state.state_broadcast_mailbox = Some(mailbox.clone());
    state.warm_state_projection_cache();
    (state, session, mailbox)
}

fn metadata_commit(state: &AppState) -> u64 {
    let mut inner = state.inner.lock().unwrap();
    state.commit_locked(&mut inner).unwrap();
    inner.revision
}

fn current_cut(state: &AppState) -> ImmutableStateSnapshot {
    let inner = state.inner.lock().unwrap();
    state.immutable_state_snapshot_locked(&inner)
}

fn materialize(work: StateBroadcastWork) -> StateResponse {
    let StateBroadcastWork::ImmutableSnapshot(snapshot) = work else {
        panic!("expected immutable metadata cut");
    };
    snapshot.materialize()
}

struct SummaryObserver;
impl SummaryObserver {
    fn install(count: Arc<AtomicUsize>) -> Self {
        TEST_BEFORE_SESSION_SUMMARY_PROJECT.with(|hook| {
            assert!(hook.borrow().is_none());
            *hook.borrow_mut() = Some(Box::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
            }));
        });
        Self
    }
}
impl Drop for SummaryObserver {
    fn drop(&mut self) {
        TEST_BEFORE_SESSION_SUMMARY_PROJECT.with(|hook| {
            hook.borrow_mut().take();
        });
    }
}

#[test]
fn general_commit_rebuilds_only_changed_summaries_and_matches_fresh_projection() {
    let (state, session, mailbox) = cached_state();
    for _ in 0..5 {
        test_session_id(&state, Agent::Codex);
    }
    state.warm_state_projection_cache();
    mailbox.take_pending_for_test();
    let count = Arc::new(AtomicUsize::new(0));
    let observer = SummaryObserver::install(count.clone());
    metadata_commit(&state);
    assert_eq!(
        count.load(Ordering::SeqCst),
        0,
        "unchanged sessions must reuse their projections"
    );
    {
        let mut inner = state.inner.lock().unwrap();
        let record = inner.session_mut(&session).unwrap();
        record.session.name = "Changed metadata".to_owned();
        record.orchestrator_auto_dispatch_blocked = true;
        state.commit_locked(&mut inner).unwrap();
    }
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "one changed session rebuilds once, not all six"
    );
    metadata_commit(&state);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    drop(observer);
    let cut = materialize(mailbox.take_pending_for_test().pop().unwrap());
    let fresh = state.snapshot_from_inner(&state.inner.lock().unwrap());
    assert_eq!(
        serde_json::to_string(&cut).unwrap(),
        serde_json::to_string(&fresh).unwrap()
    );
}

#[test]
fn capacity_preparation_invalidates_a_warm_summary_before_terminal_finalization() {
    let (state, session, mailbox) = cached_state();
    let (runtime, _input_rx, process) = test_shared_codex_runtime("projection-capacity");
    let token = RuntimeToken::Codex(runtime.runtime_id.clone());
    {
        let mut inner = state.inner.lock().unwrap();
        let record = inner.session_mut(&session).unwrap();
        record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session.clone(),
            }),
        });
        record.active_turn_generation = 1;
    }
    let before = current_cut(&state).materialize();
    assert!(!before.sessions[0].queue_paused);
    state
        .prepare_provider_capacity_failure(
            &session,
            &token,
            Some(1),
            true,
            &json!({"codexErrorInfo": "serverOverloaded", "message": "at capacity"}),
            false,
        )
        .expect("the real preparation path arms the capacity hold");
    assert_ne!(
        state.inner.lock().unwrap().sessions[0].mutation_stamp,
        before.sessions[0].session_mutation_stamp.unwrap(),
        "preparation must also expose its change to stamp-based consumers"
    );
    // This is another producer's general commit, before terminal finalization.
    metadata_commit(&state);
    let cached = materialize(mailbox.take_pending_for_test().pop().unwrap());
    let fresh = state.snapshot_from_inner(&state.inner.lock().unwrap());
    assert!(fresh.sessions[0].queue_paused);
    assert_eq!(
        cached.sessions[0].queue_paused,
        fresh.sessions[0].queue_paused
    );
    assert_ne!(
        before.sessions[0].queue_projection_hash,
        fresh.sessions[0].queue_projection_hash
    );
    assert_eq!(
        serde_json::to_string(&cached).unwrap(),
        serde_json::to_string(&fresh).unwrap()
    );
}

#[test]
fn direct_publish_then_body_sequence_finish_invalidates_a_warm_summary() {
    let (state, session, mailbox) = cached_state();
    let (stamp, sequence) = {
        let mut inner = state.inner.lock().unwrap();
        let record = inner.session_mut(&session).unwrap();
        record.mark_body_changed("body");
        let stamp = record.mutation_stamp;
        let sequence = record.wire_body_seq().unwrap();
        // Direct publication does not finish pending body-sequence work.
        state.publish_state_locked(&inner);
        (stamp, sequence)
    };
    let before = materialize(mailbox.take_pending_for_test().pop().unwrap());
    metadata_commit(&state);
    let after = materialize(mailbox.take_pending_for_test().pop().unwrap());
    let inner = state.inner.lock().unwrap();
    assert_eq!(inner.sessions[0].mutation_stamp, stamp);
    assert_eq!(inner.sessions[0].wire_body_seq(), Some(sequence + 1));
    let fresh = state.snapshot_from_inner(&inner);
    assert_eq!(before.sessions[0].body_seq, Some(sequence));
    assert_eq!(after.sessions[0].body_seq, Some(sequence + 1));
    assert_eq!(
        serde_json::to_string(&after).unwrap(),
        serde_json::to_string(&fresh).unwrap()
    );
}

#[test]
fn unstamped_metadata_writers_cannot_reuse_stale_cached_summaries() {
    let (state, session, mailbox) = cached_state();
    let mutations: &[(&str, fn(&mut SessionRecord))] = &[
        ("name", |r| r.session.name = "Unstamped rename".to_owned()),
        ("preview", |r| {
            r.session.preview = "Unstamped preview".to_owned()
        }),
        ("status", |r| r.session.status = SessionStatus::Stopping),
        ("settings", |r| {
            r.session.model = "Unstamped model".to_owned()
        }),
        ("identity", |r| {
            r.session.external_session_id = Some("new-thread".to_owned())
        }),
        ("remote owner", |r| {
            r.remote_id = Some("other-host".to_owned())
        }),
        ("readiness", |r| r.engram_boot_recovery_pending = true),
        ("capacity", |r| r.session.provider_capacity_waiting = true),
        ("source tracking", |r| {
            r.engram.source_observation_delete_requested = true
        }),
        ("pause", |r| r.set_auto_dispatch_blocked(true)),
        ("queue identity", |r| {
            r.session.pending_prompts.push(PendingPrompt {
                id: "unstamped-prompt".to_owned(),
                text: "retained".to_owned(),
                timestamp: stamp_now(),
                attachments: Vec::new(),
                expanded_text: None,
                source: None,
                engram_interrupted: false,
                is_engram_retained: false,
            })
        }),
    ];
    let original = state.inner.lock().unwrap().sessions[0].clone();
    for (label, mutate) in mutations {
        {
            let mut inner = state.inner.lock().unwrap();
            inner.sessions[0] = original.clone();
        }
        let old = current_cut(&state);
        let old_json = serde_json::to_string(old.sessions[0].as_ref()).unwrap();
        {
            let mut inner = state.inner.lock().unwrap();
            // Deliberately bypass all stamping helpers: the cache must recover
            // even if a new producer forgets the mutation convention.
            mutate(&mut inner.sessions[0]);
            assert_eq!(inner.sessions[0].mutation_stamp, original.mutation_stamp);
        }
        metadata_commit(&state);
        let cached = materialize(mailbox.take_pending_for_test().pop().unwrap());
        let fresh = state.snapshot_from_inner(&state.inner.lock().unwrap());
        assert_ne!(
            old_json,
            serde_json::to_string(&fresh.sessions[0]).unwrap(),
            "{label}"
        );
        assert_eq!(
            serde_json::to_string(&cached).unwrap(),
            serde_json::to_string(&fresh).unwrap(),
            "{label}"
        );
        assert_eq!(
            serde_json::to_string(old.sessions[0].as_ref()).unwrap(),
            old_json,
            "{label}"
        );
    }
    assert_eq!(original.session.id, session);
}

#[test]
fn immutable_cuts_keep_exact_commit_order_and_cannot_change_in_flight() {
    let (state, session, mailbox) = cached_state();
    let first = metadata_commit(&state);
    let before_name = current_cut(&state).sessions[0].name.clone();
    state.append_text_delta(&session, "body", " next").unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        inner.session_mut(&session).unwrap().session.name = "Later name".to_owned();
        state.commit_locked(&mut inner).unwrap();
    }
    let mut work = mailbox.take_pending_for_test().into_iter();
    let old = materialize(work.next().unwrap());
    let delta = work.next().unwrap();
    let new = materialize(work.next().unwrap());
    assert_eq!(old.revision, first);
    assert_eq!(old.sessions[0].name, before_name);
    assert_eq!(delta.revision(), Some(first + 1));
    assert_eq!(new.revision, first + 2);
    assert_eq!(new.sessions[0].name, "Later name");
    assert_eq!(
        new.sessions[0].body_seq,
        old.sessions[0].body_seq.map(|seq| seq + 1)
    );
    assert!(work.next().is_none());
}

#[test]
fn cache_prunes_hidden_and_deleted_records_and_restamps_recreated_ids() {
    let (state, session, _) = cached_state();
    let other = test_session_id(&state, Agent::Codex);
    let old = current_cut(&state);
    let saved = {
        let mut inner = state.inner.lock().unwrap();
        let saved = inner.session_mut(&session).unwrap().clone();
        inner.session_mut(&session).unwrap().hidden = true;
        inner.sessions.retain(|record| record.session.id != other);
        saved
    };
    let empty = current_cut(&state);
    assert!(empty.sessions.is_empty());
    assert!(
        state
            .inner
            .lock()
            .unwrap()
            .state_projection_cache
            .lock()
            .unwrap()
            .sessions
            .is_empty()
    );
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index] = saved;
        inner.stamp_session_at_index(index).unwrap();
        inner.session_mut_by_index(index).unwrap().session.name = "Recreated".to_owned();
    }
    let new = current_cut(&state).materialize();
    assert_eq!(new.sessions.len(), 1);
    assert_eq!(new.sessions[0].name, "Recreated");
    assert_eq!(
        old.sessions.len(),
        2,
        "old cuts retain their own immutable membership"
    );
    let fresh = state.snapshot_from_inner(&state.inner.lock().unwrap());
    assert_eq!(
        serde_json::to_string(&new).unwrap(),
        serde_json::to_string(&fresh).unwrap()
    );
}

#[test]
fn instance_epoch_changes_refresh_an_unchanged_records_projection() {
    let (mut state, _, _) = cached_state();
    let old = current_cut(&state);
    state.server_instance_id = "replacement-instance".to_owned();
    let new = current_cut(&state).materialize();
    assert_eq!(
        new.sessions[0].body_seq_epoch.as_deref(),
        Some("replacement-instance")
    );
    assert_ne!(
        old.sessions[0].body_seq_epoch,
        new.sessions[0].body_seq_epoch
    );
}

#[test]
fn warm_projection_scan_reports_measurement_at_live_session_count() {
    let (state, _, _) = cached_state();
    {
        let mut inner = state.inner.lock().unwrap();
        let template = inner.sessions[0].clone();
        for index in 1..6793 {
            let mut record = template.clone();
            record.session.id = format!("projection-fixture-{index}");
            inner.push_session(record);
        }
    }
    let warm_started = std::time::Instant::now();
    state.warm_state_projection_cache();
    let warm_micros = warm_started.elapsed().as_micros();
    let projections = Arc::new(AtomicUsize::new(0));
    let observer = SummaryObserver::install(projections.clone());
    let mut micros = Vec::new();
    let mut cuts = Vec::new();
    for _ in 0..3 {
        let inner = state.inner.lock().unwrap();
        let started = std::time::Instant::now();
        let cut = state.immutable_state_snapshot_locked(&inner);
        micros.push(started.elapsed().as_micros());
        drop(inner);
        assert_eq!(cut.sessions.len(), 6793);
        cuts.push(cut);
    }
    assert_eq!(projections.load(Ordering::SeqCst), 0);
    drop(observer);
    eprintln!(
        "projection_measurement sessions=6793 warm_us={warm_micros} cached_lock_us={micros:?} cached_summaries=6793"
    );
    assert_eq!(cuts.len(), 3);
}

#[test]
fn reloaded_state_does_not_reuse_a_previous_domains_same_stamp() {
    let (state, session, _) = cached_state();
    let mut record = state.inner.lock().unwrap().sessions[0].clone();
    let mut restored = StateInner::new();
    record.session.name = "Restored same stamp".to_owned();
    restored.sessions.push(record);
    *state.inner.lock().unwrap() = restored;
    let cut = current_cut(&state).materialize();
    assert_eq!(cut.sessions[0].id, session);
    assert_eq!(cut.sessions[0].name, "Restored same stamp");
}

#[test]
fn dropping_the_owner_joins_and_flushes_the_final_lone_commit() {
    let (mut state, _, mailbox) = cached_state();
    let mut stream = state.subscribe_stream_events();
    let worker = Arc::new(StateBroadcastWorker::start(state.clone()));
    let weak = Arc::downgrade(&worker);
    state.state_broadcast_worker = Some(worker);
    let revision = metadata_commit(&state);
    drop(state);
    assert!(weak.upgrade().is_none());
    assert!(mailbox.is_stopped());
    let SseStreamEvent::State(payload) = stream.try_recv().unwrap() else {
        panic!("final state was not flushed");
    };
    assert_eq!(
        serde_json::from_str::<Value>(&payload).unwrap()["revision"],
        revision
    );
}

#[test]
fn closing_accepts_late_work_and_stopped_fallback_cannot_overtake_it() {
    let (mut state, session, mailbox) = cached_state();
    let mut stream = state.subscribe_stream_events();
    let (entered, forwarding) = mpsc::channel();
    let (release, released) = mpsc::channel();
    *mailbox.before_forward.lock().unwrap() = Some(Box::new(move || {
        entered.send(()).unwrap();
        released.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
    }));
    state.state_broadcast_worker = Some(Arc::new(StateBroadcastWorker::start(state.clone())));
    let first = metadata_commit(&state);
    forwarding.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
    mailbox.begin_shutdown();
    state.append_text_delta(&session, "body", " late").unwrap();
    let last = metadata_commit(&state);
    release.send(()).unwrap();
    state.shutdown_state_broadcast_blocking();
    assert!(mailbox.is_stopped());
    let fallback = metadata_commit(&state);
    let revisions = (0..4)
        .map(|_| match stream.try_recv().unwrap() {
            SseStreamEvent::State(payload) => ("state", payload),
            SseStreamEvent::StateDiff(payload) => ("stateDiff", payload),
            SseStreamEvent::Delta(payload) => ("delta", payload),
            SseStreamEvent::LegacyState(_) => panic!("no legacy connection is counted"),
            SseStreamEvent::Lagged => panic!("no loss in this schedule"),
        })
        .map(|(kind, payload)| {
            let revision = serde_json::from_str::<Value>(&payload).unwrap()["revision"]
                .as_u64()
                .unwrap();
            (kind, revision)
        })
        .collect::<Vec<_>>();
    // Closing keeps the base: the late cut still diffs from the first one.
    // After Stopped, the synchronous fallback is a full state again.
    assert_eq!(
        revisions,
        vec![
            ("state", first),
            ("delta", first + 1),
            ("stateDiff", last),
            ("state", fallback)
        ]
    );
}

#[tokio::test]
async fn fresh_subscription_uses_current_state_despite_older_queued_capture() {
    let (state, _, mailbox) = cached_state();
    let old = metadata_commit(&state);
    let queued = mailbox.recv_next();
    let mut stream = state.subscribe_stream_events();
    let current = metadata_commit(&state);
    let initial = state_snapshot_payload_for_sse(state.clone()).await;
    assert_eq!(
        serde_json::from_str::<Value>(&initial).unwrap()["revision"],
        current
    );
    forward_state_broadcast_work(queued, &state.state_broadcast_senders);
    let SseStreamEvent::State(payload) = stream.try_recv().unwrap() else {
        panic!("queued capture expected");
    };
    assert_eq!(
        serde_json::from_str::<Value>(&payload).unwrap()["revision"],
        old
    );
}
