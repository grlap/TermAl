// Owns observable queue-hash compatibility and warm-cut validation regressions.
// Does not own dispatch, retention policy, broadcaster ordering or live timing.
// New module using the deferred_state_snapshot fixture and production cuts.
use super::*;

struct HashObserver;

impl HashObserver {
    fn install(count: Arc<AtomicUsize>) -> Self {
        TEST_BEFORE_QUEUE_PROJECTION_HASH.with(|hook| {
            assert!(hook.borrow().is_none());
            *hook.borrow_mut() = Some(Box::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
            }));
        });
        Self
    }
}

impl Drop for HashObserver {
    fn drop(&mut self) {
        TEST_BEFORE_QUEUE_PROJECTION_HASH.with(|hook| {
            hook.borrow_mut().take();
        });
    }
}

fn prompt(id: &str) -> PendingPrompt {
    PendingPrompt {
        id: id.to_owned(),
        text: "private queued body".repeat(100),
        timestamp: stamp_now(),
        attachments: Vec::new(),
        expanded_text: None,
        source: None,
        engram_interrupted: false,
        is_engram_retained: false,
    }
}

pub(super) fn queued(id: &str) -> QueuedPromptRecord {
    QueuedPromptRecord {
        pending_prompt: prompt(id),
        attachments: Vec::new(),
        source: QueuedPromptSource::User,
        engram_ordinary_handoff: false,
        promoted_message_index: None,
        promotion_disposition_known: true,
        engram_waiting: false,
        engram_bind: None,
        engram_evaluate: None,
        engram_interrupted: false,
    }
}

#[test]
fn public_queue_hash_keeps_its_ordered_identity_and_disposition_contract() {
    let (state, _, _) = cached_state();
    {
        let mut inner = state.inner.lock().unwrap();
        let record = &mut inner.sessions[0];
        let mut second = prompt("second");
        second.engram_interrupted = true;
        second.is_engram_retained = true;
        record.session.pending_prompts = vec![prompt("first"), second];
    }
    let embedded = current_cut(&state).materialize();
    let expected = sha256_hex(br#"[false,[["first",false,false],["second",true,true]]]"#);
    assert_eq!(
        embedded.sessions[0].queue_projection_hash.as_deref(),
        Some(expected.as_str())
    );
    {
        let mut inner = state.inner.lock().unwrap();
        let record = &mut inner.sessions[0];
        // The authority has different text from its embedded projection. Only
        // its identity and derived public disposition determine the hash.
        let mut second = queued("second");
        second.engram_ordinary_handoff = true;
        record.queued_prompts = vec![queued("first"), second].into();
        record.session.pending_prompts[0].id = "stale-embedded-id".to_owned();
    }
    let authoritative = current_cut(&state).materialize();
    assert_eq!(
        authoritative.sessions[0].queue_projection_hash,
        embedded.sessions[0].queue_projection_hash
    );
    {
        let mut inner = state.inner.lock().unwrap();
        inner.sessions[0].queued_prompts[0]
            .pending_prompt
            .text
            .push_str(" body-only change");
    }
    assert_eq!(
        current_cut(&state).materialize().sessions[0].queue_projection_hash,
        authoritative.sessions[0].queue_projection_hash
    );
}

#[test]
fn warm_cuts_reuse_queue_hashes_and_only_rebuild_changed_summaries() {
    let (state, session, mailbox) = cached_state();
    let count = Arc::new(AtomicUsize::new(0));
    let _observer = HashObserver::install(count.clone());
    for _ in 0..3 {
        metadata_commit(&state);
    }
    assert_eq!(
        count.load(Ordering::SeqCst),
        0,
        "warm cuts must not hash unchanged queues"
    );
    {
        let mut inner = state.inner.lock().unwrap();
        inner
            .session_mut(&session)
            .unwrap()
            .session
            .pending_prompts
            .push(prompt("new"));
        state.commit_locked(&mut inner).unwrap();
    }
    let rebuilt = count.load(Ordering::SeqCst);
    assert!(rebuilt > 0, "changed queues must construct a new hash");
    metadata_commit(&state);
    assert_eq!(count.load(Ordering::SeqCst), rebuilt);
    let cached = materialize(mailbox.take_pending_for_test().pop().unwrap());
    let fresh = state.snapshot_from_inner(&state.inner.lock().unwrap());
    assert!(
        count.load(Ordering::SeqCst) > rebuilt,
        "fresh projection is the positive control"
    );
    assert_eq!(
        serde_json::to_value(cached).unwrap(),
        serde_json::to_value(fresh).unwrap()
    );
}

#[test]
fn unstamped_authoritative_queue_changes_refresh_hashes_without_changing_old_cuts() {
    let (state, _, _) = cached_state();
    let mut baseline = state.inner.lock().unwrap().sessions[0].clone();
    baseline.queued_prompts = vec![queued("first"), queued("second")].into();
    baseline.session.pending_prompts = vec![prompt("embedded-fallback")];
    let mutations: &[(&str, fn(&mut SessionRecord))] = &[
        ("identity", |r| {
            r.queued_prompts[0].pending_prompt.id = "replacement".to_owned()
        }),
        ("order", |r| r.queued_prompts.swap(0, 1)),
        ("removed", |r| {
            r.queued_prompts.pop_back();
        }),
        ("fallback", |r| r.queued_prompts.clear()),
        ("interrupted", |r| {
            r.queued_prompts[0].engram_interrupted = true
        }),
        ("handoff", |r| {
            r.queued_prompts[0].engram_ordinary_handoff = true
        }),
        ("legacy", |r| {
            r.queued_prompts[0].promotion_disposition_known = false
        }),
        ("waiting", |r| r.queued_prompts[0].engram_waiting = true),
        ("binding", |r| {
            r.queued_prompts[0].engram_bind = Some(json!({}))
        }),
        ("evaluation", |r| {
            r.queued_prompts[0].engram_evaluate = Some(json!({}))
        }),
        ("promoted", |r| {
            r.queued_prompts[0].promoted_message_index = Some(0)
        }),
        ("pause", |r| r.orchestrator_auto_dispatch_blocked = true),
    ];
    for (label, mutate) in mutations {
        state.inner.lock().unwrap().sessions[0] = baseline.clone();
        let old = current_cut(&state);
        let old_hash = old.sessions[0].queue_projection_hash.clone();
        {
            let mut inner = state.inner.lock().unwrap();
            mutate(&mut inner.sessions[0]);
            assert_eq!(inner.sessions[0].mutation_stamp, baseline.mutation_stamp);
        }
        let cut = current_cut(&state).materialize();
        let fresh = state.snapshot_from_inner(&state.inner.lock().unwrap());
        assert_ne!(cut.sessions[0].queue_projection_hash, old_hash, "{label}");
        assert_eq!(
            serde_json::to_value(cut).unwrap(),
            serde_json::to_value(fresh).unwrap(),
            "{label}"
        );
        assert_eq!(old.sessions[0].queue_projection_hash, old_hash, "{label}");
    }
}

#[test]
fn unstamped_embedded_queue_dispositions_refresh_a_warm_cut() {
    let (state, _, _) = cached_state();
    let mut baseline = state.inner.lock().unwrap().sessions[0].clone();
    baseline.session.pending_prompts = vec![prompt("first"), prompt("second")];
    let mutations: &[(&str, fn(&mut SessionRecord))] = &[
        ("identity", |r| {
            r.session.pending_prompts[0].id = "replacement".to_owned()
        }),
        ("order", |r| r.session.pending_prompts.swap(0, 1)),
        ("interrupted", |r| {
            r.session.pending_prompts[0].engram_interrupted = true
        }),
        ("retained", |r| {
            r.session.pending_prompts[0].is_engram_retained = true
        }),
        ("empty", |r| r.session.pending_prompts.clear()),
    ];
    for (label, mutate) in mutations {
        state.inner.lock().unwrap().sessions[0] = baseline.clone();
        let old = current_cut(&state);
        let old_hash = old.sessions[0].queue_projection_hash.clone();
        {
            let mut inner = state.inner.lock().unwrap();
            mutate(&mut inner.sessions[0]);
            assert_eq!(inner.sessions[0].mutation_stamp, baseline.mutation_stamp);
        }
        let cut = current_cut(&state).materialize();
        let fresh = state.snapshot_from_inner(&state.inner.lock().unwrap());
        assert_ne!(cut.sessions[0].queue_projection_hash, old_hash, "{label}");
        assert_eq!(
            serde_json::to_value(cut).unwrap(),
            serde_json::to_value(fresh).unwrap(),
            "{label}"
        );
        assert_eq!(old.sessions[0].queue_projection_hash, old_hash, "{label}");
    }
}

#[test]
fn repairing_an_unstamped_partial_identity_rebuilds_an_empty_local_queue() {
    let (state, _, _) = cached_state();
    let (stamp, sequence) = {
        let mut inner = state.inner.lock().unwrap();
        let record = &mut inner.sessions[0];
        let sequence = record.wire_body_seq();
        // Align every existing sequence/epoch guard so locality is the only
        // input that can distinguish this partial identity from its repair.
        record.session.body_seq = sequence;
        record.session.body_seq_epoch = Some(state.server_instance_id.clone());
        record.body_sequence.remote_applied = sequence;
        record.body_sequence.remote_epoch = Some(state.server_instance_id.clone());
        record.remote_session_id = Some("partial-identity".to_owned());
        record.session.queue_projection_hash = None;
        assert!(!record.is_local_session());
        assert!(!record.is_remote_proxy());
        (record.mutation_stamp, sequence)
    };
    let before = current_cut(&state);
    assert_eq!(before.sessions[0].queue_projection_hash, None);
    {
        let mut inner = state.inner.lock().unwrap();
        let record = &mut inner.sessions[0];
        record.remote_session_id = None;
        assert!(record.is_local_session());
        assert_eq!(record.mutation_stamp, stamp);
        assert_eq!(record.wire_body_seq(), sequence);
    }
    let repaired = current_cut(&state).materialize();
    let fresh = state.snapshot_from_inner(&state.inner.lock().unwrap());
    assert!(repaired.sessions[0].queue_projection_hash.is_some());
    assert_eq!(
        serde_json::to_value(repaired).unwrap(),
        serde_json::to_value(fresh).unwrap()
    );
}

#[test]
fn remote_queue_hash_remains_remote_authority_even_with_an_embedded_queue() {
    let (state, _, _) = cached_state();
    {
        let mut inner = state.inner.lock().unwrap();
        let record = &mut inner.sessions[0];
        record.remote_id = Some("remote-host".to_owned());
        record.remote_session_id = Some("remote-session".to_owned());
        record.session.pending_prompts = vec![prompt("remote-prompt")];
        record.session.queue_projection_hash = Some("remote-hash-before".to_owned());
    }
    let old = current_cut(&state);
    state.inner.lock().unwrap().sessions[0]
        .session
        .queue_projection_hash = Some("remote-hash-after".to_owned());
    let new = current_cut(&state).materialize();
    assert_eq!(
        old.sessions[0].queue_projection_hash.as_deref(),
        Some("remote-hash-before")
    );
    assert_eq!(
        new.sessions[0].queue_projection_hash.as_deref(),
        Some("remote-hash-after")
    );
}
