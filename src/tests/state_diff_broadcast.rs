//! The broadcaster's `stateDiff` producer: cumulative diffs from the last
//! emitted cut, full states whenever no base exists, and a paired full state
//! only while legacy connections are counted. The producer is driven on the
//! test thread, so every emission is observed in order without a worker.

use super::*;

const DIFF_INSTANCE: &str = "diff-instance";

fn diff_senders() -> StateBroadcastSenders {
    StateBroadcastSenders {
        stream_events: broadcast::channel(64).0,
        legacy_state_subscribers: Arc::default(),
        full_state_request_generation: Arc::default(),
        diff_counters: Arc::default(),
        state_events: broadcast::channel(16).0,
        delta_events: broadcast::channel(16).0,
        state_diff_events: broadcast::channel(16).0,
    }
}

fn template_summary() -> StateSessionSummary {
    let (state, _, _) = cached_state();
    (*current_cut(&state).sessions[0]).clone()
}

fn summary(template: &StateSessionSummary, id: &str) -> Arc<StateSessionSummary> {
    let mut summary = template.clone();
    summary.id = id.to_owned();
    summary.name = format!("Session {id}");
    Arc::new(summary)
}

fn cut(revision: u64, sessions: &[&Arc<StateSessionSummary>]) -> ImmutableStateSnapshot {
    let mut header = empty_state_events_response(DIFF_INSTANCE.to_owned());
    header.revision = revision;
    ImmutableStateSnapshot {
        header,
        sessions: sessions.iter().map(|session| Arc::clone(*session)).collect(),
        request_generation: 0,
        before_materialize: None,
    }
}

fn delegation(id: &str, child: &str) -> DelegationStateSummary {
    serde_json::from_value(json!({
        "id": id,
        "childSessionId": child,
        "mode": "reviewer",
        "reviewResultRequired": true,
    }))
    .expect("a delegation row")
}

fn test_run(run_id: &str, state: &str) -> TestRunSummary {
    serde_json::from_value(json!({
        "runId": run_id,
        "projectId": null,
        "worktree": "C:/worktree",
        "runDir": "C:/worktree/.git/review-runs/run",
        "preset": "focused",
        "command": null,
        "commandTruncated": false,
        "detached": null,
        "state": state,
        "interrupted": false,
        "currentStage": null,
        "stages": [],
        "ownerSessionId": "owner",
        "notifyTo": null,
        "notifySessionId": null,
        "startedAt": null,
        "endedAt": null,
        "exitCode": null,
        "error": null,
        "detailVersion": "sha256:d1d1d1"
    }))
    .expect("a run row")
}

/// Forwards a synthetic cut as if captured just now: it carries the current
/// full-state request generation, as a real capture under StateInner does.
fn forward_cut(
    producer: &mut StateDiffProducer,
    senders: &StateBroadcastSenders,
    mut snapshot: ImmutableStateSnapshot,
) {
    snapshot.request_generation = senders
        .full_state_request_generation
        .load(Ordering::SeqCst);
    producer.forward(StateBroadcastWork::ImmutableSnapshot(snapshot), senders);
}

fn parse_payload(payload: &str) -> Value {
    serde_json::from_str(payload).unwrap()
}

fn emitted(receiver: &mut broadcast::Receiver<SseStreamEvent>) -> Vec<(&'static str, Value)> {
    let mut events = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        events.push(match event {
            SseStreamEvent::State(payload) => ("state", parse_payload(&payload)),
            SseStreamEvent::LegacyState(payload) => ("legacyState", parse_payload(&payload)),
            SseStreamEvent::StateDiff(payload) => ("stateDiff", parse_payload(&payload)),
            SseStreamEvent::Delta(payload) => ("delta", parse_payload(&payload)),
            SseStreamEvent::Lagged => ("lagged", Value::Null),
        });
    }
    events
}

fn kinds(events: &[(&'static str, Value)]) -> Vec<&'static str> {
    events.iter().map(|(kind, _)| *kind).collect()
}

fn only_diff(receiver: &mut broadcast::Receiver<SseStreamEvent>) -> Value {
    let events = emitted(receiver);
    assert_eq!(kinds(&events), ["stateDiff"], "{events:?}");
    events.into_iter().next().unwrap().1
}

fn ids(rows: &Value, key: &str) -> Vec<String> {
    rows.as_array()
        .unwrap()
        .iter()
        .map(|row| row[key].as_str().unwrap().to_owned())
        .collect()
}

fn keys(rows: &Value) -> Vec<String> {
    rows.as_array()
        .unwrap()
        .iter()
        .map(|key| key.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn state_diff_without_a_base_sends_a_full_state_then_diffs_from_it() {
    let template = template_summary();
    let (a, b) = (summary(&template, "a"), summary(&template, "b"));
    let senders = diff_senders();
    let mut receiver = senders.stream_events.subscribe();
    let mut producer = StateDiffProducer::default();

    forward_cut(&mut producer, &senders, cut(10, &[&a, &b]));
    let events = emitted(&mut receiver);
    assert_eq!(kinds(&events), ["state"], "worker start has no base");
    assert_eq!(events[0].1["revision"], 10);
    assert_eq!(events[0].1["stateDiffBase"], true, "a worker base is marked");

    let b_changed = Arc::new(StateSessionSummary {
        name: "Renamed".to_owned(),
        ..(*b).clone()
    });
    let mut updated_cut = cut(11, &[&a, &b_changed]);
    updated_cut.header.engram_control_recovery = vec![EngramControlRecoverySummary {
        project_id: "project".to_owned(),
        session_id: "a".to_owned(),
        operation: "turn_checkpoint".to_owned(),
        code: "checkpoint_failed".to_owned(),
        excluded: true,
        settlement_confirmed: false,
        next_actions: vec!["inspect_session".to_owned()],
    }];
    let expected_recovery =
        serde_json::to_value(&updated_cut.header.engram_control_recovery).unwrap();
    forward_cut(&mut producer, &senders, updated_cut);
    let diff = only_diff(&mut receiver);
    assert_eq!(diff["baseRevision"], 10);
    assert_eq!(diff["revision"], 11);
    assert_eq!(diff["serverInstanceId"], DIFF_INSTANCE);
    assert_eq!(ids(&diff["sessions"]["changed"], "id"), ["b"]);
    assert_eq!(diff["sessions"]["changed"][0]["name"], "Renamed");
    assert!(keys(&diff["sessions"]["removed"]).is_empty());
    assert!(diff["sessions"].get("order").is_none());
    // Every header slice is absolute in every diff, even when empty.
    for slice in [
        "engramControlRecovery",
        "codex",
        "agentReadiness",
        "preferences",
        "projects",
        "orchestrators",
        "workspaces",
        "delegationWaits",
        "testRunWaits",
        "pendingEngramMcpRevocationSessionIds",
    ] {
        assert!(diff["header"].get(slice).is_some(), "header slice {slice}");
    }
    let full = serde_json::to_value(cut(11, &[&a, &b_changed]).materialize()).unwrap();
    assert_eq!(diff["header"]["preferences"], full["preferences"]);
    assert_eq!(diff["header"]["codex"], full["codex"]);
    assert_eq!(diff["header"]["engramControlRecovery"], expected_recovery);
    assert_eq!(diff["delegations"], json!({"changed": [], "removed": []}));
    assert_eq!(diff["testRuns"], json!({"changed": [], "removed": []}));

    forward_cut(&mut producer, &senders, cut(12, &[&a, &b_changed]));
    let cleared = only_diff(&mut receiver);
    assert_eq!(cleared["baseRevision"], 11);
    assert_eq!(cleared["header"]["engramControlRecovery"], json!([]));
}

#[test]
fn state_diff_uses_arc_identity_not_value_equality_for_sessions() {
    let template = template_summary();
    let (a, b) = (summary(&template, "a"), summary(&template, "b"));
    let senders = diff_senders();
    let mut receiver = senders.stream_events.subscribe();
    let mut producer = StateDiffProducer::default();
    forward_cut(&mut producer, &senders, cut(1, &[&a, &b]));
    emitted(&mut receiver);

    // A value-equal summary in a new Arc was captured anew: it goes out.
    let b_recaptured = Arc::new((*b).clone());
    forward_cut(&mut producer, &senders, cut(2, &[&a, &b_recaptured]));
    let diff = only_diff(&mut receiver);
    assert_eq!(ids(&diff["sessions"]["changed"], "id"), ["b"]);
    assert_eq!(
        diff["sessions"]["changed"][0],
        serde_json::to_value(b.as_ref()).unwrap()
    );

    // The same Arcs as the last emitted cut carry nothing.
    forward_cut(&mut producer, &senders, cut(3, &[&a, &b_recaptured]));
    let diff = only_diff(&mut receiver);
    assert_eq!(diff["baseRevision"], 2);
    assert!(ids(&diff["sessions"]["changed"], "id").is_empty());
}

#[test]
fn state_diff_through_the_cache_reports_a_reverted_session_captured_in_between() {
    let (state, session, mailbox) = cached_state();
    let mut receiver = state.subscribe_stream_events();
    let mut producer = StateDiffProducer::default();
    let forward_pending = |producer: &mut StateDiffProducer| {
        let mut pending = mailbox.take_pending_for_test();
        assert_eq!(pending.len(), 1);
        producer.forward(pending.pop().unwrap(), &state.state_broadcast_senders);
    };
    let base_revision = metadata_commit(&state);
    forward_pending(&mut producer);
    let events = emitted(&mut receiver);
    assert_eq!(kinds(&events), ["state"]);
    let base_session = events[0].1["sessions"][0].clone();
    let original = state.inner.lock().unwrap().sessions[0].session.name.clone();
    let rename = |name: &str| {
        // Unstamped on purpose: the cache's value check mints the new Arc.
        state.inner.lock().unwrap().sessions[0].session.name = name.to_owned();
    };

    // Changed and reverted with no capture in between: the cache still holds
    // the base's Arc, and no client ever saw the intermediate value.
    rename("Intermediate");
    rename(&original);
    let quiet = metadata_commit(&state);
    forward_pending(&mut producer);
    let diff = only_diff(&mut receiver);
    assert_eq!((diff["baseRevision"].as_u64(), diff["revision"].as_u64()), (Some(base_revision), Some(quiet)));
    assert!(ids(&diff["sessions"]["changed"], "id").is_empty());

    // Changed, captured, then reverted: the reverted value is a new Arc and
    // goes out absolutely even though it equals the emitted base.
    rename("Intermediate");
    drop(current_cut(&state));
    rename(&original);
    let reverted = metadata_commit(&state);
    forward_pending(&mut producer);
    let diff = only_diff(&mut receiver);
    assert_eq!((diff["baseRevision"].as_u64(), diff["revision"].as_u64()), (Some(quiet), Some(reverted)));
    assert_eq!(ids(&diff["sessions"]["changed"], "id"), [session]);
    assert_eq!(diff["sessions"]["changed"][0], base_session);
}

#[test]
fn state_diff_after_coalesced_cuts_is_cumulative_from_the_last_emitted_cut() {
    let (state, first, mailbox) = cached_state();
    let second = test_session_id(&state, Agent::Codex);
    let mut receiver = state.subscribe_stream_events();
    let mut producer = StateDiffProducer::default();
    mailbox.take_pending_for_test();
    let base_revision = metadata_commit(&state);
    let mut pending = mailbox.take_pending_for_test();
    producer.forward(pending.pop().unwrap(), &state.state_broadcast_senders);
    assert_eq!(kinds(&emitted(&mut receiver)), ["state"]);

    for (session, name) in [(&first, "First renamed"), (&second, "Second renamed")] {
        let mut inner = state.inner.lock().unwrap();
        inner.session_mut(session).unwrap().session.name = name.to_owned();
        state.commit_locked(&mut inner).unwrap();
    }
    let mut pending = mailbox.take_pending_for_test();
    assert_eq!(pending.len(), 1, "adjacent cuts coalesce in the mailbox");
    let latest = state.inner.lock().unwrap().revision;
    producer.forward(pending.pop().unwrap(), &state.state_broadcast_senders);
    let diff = only_diff(&mut receiver);
    assert_eq!(diff["baseRevision"].as_u64(), Some(base_revision));
    assert_eq!(diff["revision"].as_u64(), Some(latest));
    let mut changed = ids(&diff["sessions"]["changed"], "id");
    changed.sort();
    let mut expected = vec![first.clone(), second.clone()];
    expected.sort();
    assert_eq!(changed, expected);
    let names: Vec<&str> = diff["sessions"]["changed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"First renamed") && names.contains(&"Second renamed"));
}

#[test]
fn state_diff_sends_delta_touched_keys_absolutely_and_absent_ones_as_removed() {
    let template = template_summary();
    let (a, b) = (summary(&template, "a"), summary(&template, "b"));
    let with_rows = |revision| {
        let mut snapshot = cut(revision, &[&a, &b]);
        snapshot.header.delegations = vec![delegation("d1", "b"), delegation("d2", "b")];
        snapshot.header.test_runs = vec![test_run("r1", "running"), test_run("r2", "running")];
        snapshot
    };
    let senders = diff_senders();
    let mut receiver = senders.stream_events.subscribe();
    let mut producer = StateDiffProducer::default();
    forward_cut(&mut producer, &senders, with_rows(1));
    emitted(&mut receiver);

    for delta in [
        DeltaEvent::ConversationMarkerDeleted {
            revision: 2,
            session_id: "a".to_owned(),
            marker_id: "marker".to_owned(),
            session_mutation_stamp: None,
        },
        DeltaEvent::DelegationCanceled {
            revision: 3,
            delegation_id: "d1".to_owned(),
            canceled_at: "now".to_owned(),
            reason: None,
        },
        DeltaEvent::TestRunChanged {
            revision: 4,
            run: test_run("r1", "running"),
        },
        DeltaEvent::TestRunWaitResumeDispatchFailed {
            revision: 5,
            session_id: "ghost".to_owned(),
            error: "cannot dispatch".to_owned(),
            server_instance_id: DIFF_INSTANCE.to_owned(),
        },
        DeltaEvent::DelegationCanceled {
            revision: 6,
            delegation_id: "ghost-delegation".to_owned(),
            canceled_at: "now".to_owned(),
            reason: None,
        },
        DeltaEvent::TestRunRemoved {
            revision: 7,
            run_id: "ghost-run".to_owned(),
        },
    ] {
        producer.forward(StateBroadcastWork::Delta(delta), &senders);
    }
    assert_eq!(kinds(&emitted(&mut receiver)), ["delta"; 6]);

    // Every row is the base's own value; only the touched keys go out.
    forward_cut(&mut producer, &senders, with_rows(8));
    let diff = only_diff(&mut receiver);
    assert_eq!(ids(&diff["sessions"]["changed"], "id"), ["a"]);
    assert_eq!(keys(&diff["sessions"]["removed"]), ["ghost"]);
    assert!(diff["sessions"].get("order").is_none());
    assert_eq!(ids(&diff["delegations"]["changed"], "id"), ["d1"]);
    assert_eq!(keys(&diff["delegations"]["removed"]), ["ghost-delegation"]);
    assert_eq!(ids(&diff["testRuns"]["changed"], "runId"), ["r1"]);
    assert_eq!(keys(&diff["testRuns"]["removed"]), ["ghost-run"]);

    // The emitted cut clears the touched keys.
    forward_cut(&mut producer, &senders, with_rows(9));
    let diff = only_diff(&mut receiver);
    assert!(ids(&diff["sessions"]["changed"], "id").is_empty());
    assert!(keys(&diff["sessions"]["removed"]).is_empty());
    assert_eq!(diff["delegations"], json!({"changed": [], "removed": []}));
    assert_eq!(diff["testRuns"], json!({"changed": [], "removed": []}));
}

#[test]
fn state_diff_rows_compare_values_and_report_keys_absent_from_the_cut() {
    let template = template_summary();
    let a = summary(&template, "a");
    let senders = diff_senders();
    let mut receiver = senders.stream_events.subscribe();
    let mut producer = StateDiffProducer::default();
    let mut base = cut(1, &[&a]);
    base.header.delegations = vec![delegation("d1", "a"), delegation("d2", "a")];
    base.header.test_runs = vec![test_run("r1", "running"), test_run("r2", "running")];
    forward_cut(&mut producer, &senders, base);
    emitted(&mut receiver);

    let mut next = cut(2, &[&a]);
    let mut d1 = delegation("d1", "a");
    d1.review_freeze_allowed = true;
    next.header.delegations = vec![d1, delegation("d3", "a")];
    next.header.test_runs = vec![test_run("r1", "passed"), test_run("r2", "running")];
    forward_cut(&mut producer, &senders, next);
    let diff = only_diff(&mut receiver);
    assert_eq!(ids(&diff["delegations"]["changed"], "id"), ["d1", "d3"]);
    assert_eq!(diff["delegations"]["changed"][0]["reviewFreezeAllowed"], true);
    assert_eq!(keys(&diff["delegations"]["removed"]), ["d2"]);
    assert_eq!(ids(&diff["testRuns"]["changed"], "runId"), ["r1"]);
    assert_eq!(diff["testRuns"]["changed"][0]["state"], "passed");
    assert!(keys(&diff["testRuns"]["removed"]).is_empty());
}

#[test]
fn state_diff_reports_removals_and_sends_order_only_when_membership_or_order_changes() {
    let template = template_summary();
    let (a, b, c, d) = (
        summary(&template, "a"),
        summary(&template, "b"),
        summary(&template, "c"),
        summary(&template, "d"),
    );
    let senders = diff_senders();
    let mut receiver = senders.stream_events.subscribe();
    let mut producer = StateDiffProducer::default();
    forward_cut(&mut producer, &senders, cut(1, &[&a, &b, &c]));
    emitted(&mut receiver);

    forward_cut(&mut producer, &senders, cut(2, &[&a, &c]));
    let diff = only_diff(&mut receiver);
    assert!(ids(&diff["sessions"]["changed"], "id").is_empty());
    assert_eq!(keys(&diff["sessions"]["removed"]), ["b"]);
    assert_eq!(keys(&diff["sessions"]["order"]), ["a", "c"]);

    forward_cut(&mut producer, &senders, cut(3, &[&c, &a]));
    let diff = only_diff(&mut receiver);
    assert!(ids(&diff["sessions"]["changed"], "id").is_empty());
    assert!(keys(&diff["sessions"]["removed"]).is_empty());
    assert_eq!(keys(&diff["sessions"]["order"]), ["c", "a"]);

    forward_cut(&mut producer, &senders, cut(4, &[&c, &a]));
    assert!(only_diff(&mut receiver)["sessions"].get("order").is_none());

    forward_cut(&mut producer, &senders, cut(5, &[&c, &a, &d]));
    let diff = only_diff(&mut receiver);
    assert_eq!(ids(&diff["sessions"]["changed"], "id"), ["d"]);
    assert_eq!(keys(&diff["sessions"]["order"]), ["c", "a", "d"]);
}

#[test]
fn state_diff_restarts_from_a_full_state_after_loss_prebuilt_snapshot_or_new_instance() {
    let template = template_summary();
    let a = summary(&template, "a");
    let senders = diff_senders();
    let mut receiver = senders.stream_events.subscribe();
    let mut producer = StateDiffProducer::default();
    forward_cut(&mut producer, &senders, cut(1, &[&a]));
    forward_cut(&mut producer, &senders, cut(2, &[&a]));
    assert_eq!(kinds(&emitted(&mut receiver)), ["state", "stateDiff"]);

    // A sticky mailbox loss discards the base and touched keys first.
    producer.forward(
        StateBroadcastWork::Delta(DeltaEvent::TestRunRemoved {
            revision: 3,
            run_id: "lost".to_owned(),
        }),
        &senders,
    );
    producer.forward(StateBroadcastWork::Lagged, &senders);
    forward_cut(&mut producer, &senders, cut(4, &[&a]));
    let events = emitted(&mut receiver);
    assert_eq!(kinds(&events), ["delta", "lagged", "state"]);
    assert_eq!(events[2].1["revision"], 4);
    assert_eq!(events[2].1["stateDiffBase"], true);
    forward_cut(&mut producer, &senders, cut(5, &[&a]));
    let diff = only_diff(&mut receiver);
    assert_eq!(diff["baseRevision"], 4);
    assert!(keys(&diff["testRuns"]["removed"]).is_empty());

    // A pre-built snapshot is a full state for everyone and leaves no base.
    let mut prebuilt = empty_state_events_response(DIFF_INSTANCE.to_owned());
    prebuilt.revision = 6;
    producer.forward(StateBroadcastWork::Snapshot(prebuilt), &senders);
    forward_cut(&mut producer, &senders, cut(7, &[&a]));
    let events = emitted(&mut receiver);
    assert_eq!(kinds(&events), ["state", "state"]);
    assert!(events[0].1.get("stateDiffBase").is_none(), "a pre-built snapshot is no base");
    assert_eq!(events[1].1["stateDiffBase"], true);

    // A different server instance cannot diff from the old one's cut.
    let mut replacement = cut(8, &[&a]);
    replacement.header.server_instance_id = "replacement-instance".to_owned();
    forward_cut(&mut producer, &senders, replacement);
    let mut following = cut(9, &[&a]);
    following.header.server_instance_id = "replacement-instance".to_owned();
    forward_cut(&mut producer, &senders, following);
    let events = emitted(&mut receiver);
    assert_eq!(kinds(&events), ["state", "stateDiff"]);
    assert_eq!(events[1].1["baseRevision"], 8);
}

#[test]
fn state_diff_failed_emission_leaves_no_base_so_the_next_cut_is_full() {
    let template = template_summary();
    let a = summary(&template, "a");
    let senders = diff_senders();
    let mut producer = StateDiffProducer::default();
    let mut receiver = senders.stream_events.subscribe();
    forward_cut(&mut producer, &senders, cut(1, &[&a]));
    assert_eq!(kinds(&emitted(&mut receiver)), ["state"]);

    // No receiver: the diff cannot be delivered, so the base does not advance.
    drop(receiver);
    forward_cut(&mut producer, &senders, cut(2, &[&a]));
    let mut receiver = senders.stream_events.subscribe();
    forward_cut(&mut producer, &senders, cut(3, &[&a]));
    let events = emitted(&mut receiver);
    assert_eq!(kinds(&events), ["state"]);
    assert_eq!(events[0].1["revision"], 3);

    // A full state that reached nobody does not become a base either.
    drop(receiver);
    let mut fresh = StateDiffProducer::default();
    forward_cut(&mut fresh, &senders, cut(4, &[&a]));
    let mut receiver = senders.stream_events.subscribe();
    forward_cut(&mut fresh, &senders, cut(5, &[&a]));
    assert_eq!(kinds(&emitted(&mut receiver)), ["state"]);
}

#[test]
fn state_diff_pairs_a_full_state_only_while_legacy_connections_are_counted() {
    let template = template_summary();
    let a = summary(&template, "a");
    let expected_full = serde_json::to_string(&cut(3, &[&a]).materialize()).unwrap();
    let senders = diff_senders();
    let mut receiver = senders.stream_events.subscribe();
    let mut producer = StateDiffProducer::default();
    forward_cut(&mut producer, &senders, cut(1, &[&a]));
    emitted(&mut receiver);

    let projections = Arc::new(AtomicUsize::new(0));
    let observer = ProjectionObserver::install(projections.clone());
    forward_cut(&mut producer, &senders, cut(2, &[&a]));
    assert_eq!(kinds(&emitted(&mut receiver)), ["stateDiff"]);
    assert_eq!(
        projections.load(Ordering::SeqCst),
        0,
        "without legacy connections a diff never materializes the full state"
    );

    let legacy = LegacyStateSubscriber::register(&senders);
    forward_cut(&mut producer, &senders, cut(3, &[&a]));
    let mut paired = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        paired.push(event);
    }
    assert_eq!(projections.load(Ordering::SeqCst), 1);
    let [SseStreamEvent::StateDiff(diff), SseStreamEvent::LegacyState(full)] = paired.as_slice()
    else {
        panic!("a counted legacy connection gets the diff's full state: {paired:?}");
    };
    assert_eq!(serde_json::from_str::<Value>(diff).unwrap()["revision"], 3);
    assert_eq!(&**full, expected_full.as_str(), "legacy payloads stay bit-identical");

    drop(legacy);
    forward_cut(&mut producer, &senders, cut(4, &[&a]));
    drop(observer);
    let diff = only_diff(&mut receiver);
    assert_eq!(diff["baseRevision"], 3, "a paired emission advances the base");
    assert_eq!(projections.load(Ordering::SeqCst), 1);
}

#[test]
fn state_diff_marked_full_state_is_todays_bytes_plus_the_mark() {
    let template = template_summary();
    let a = summary(&template, "a");
    let unmarked = serde_json::to_string(&cut(1, &[&a]).materialize()).unwrap();
    let senders = diff_senders();
    let mut receiver = senders.stream_events.subscribe();
    let mut producer = StateDiffProducer::default();
    forward_cut(&mut producer, &senders, cut(1, &[&a]));
    let Ok(SseStreamEvent::State(marked)) = receiver.try_recv() else {
        panic!("a marked full state was expected");
    };
    let expected = format!(
        "{},\"stateDiffBase\":true}}",
        unmarked.strip_suffix('}').unwrap()
    );
    assert_eq!(&*marked, expected.as_str());
    // Legacy readers (the remote hub's `StateEventPayload`, a plain
    // `StateResponse`) ignore the unknown field.
    let decoded: StateEventPayload = serde_json::from_str(&marked).unwrap();
    assert!(!decoded.sse_fallback);
    assert_eq!(decoded.state.revision, 1);
    assert_eq!(decoded.state.sessions.len(), 1);
    let plain: StateResponse = serde_json::from_str(&marked).unwrap();
    assert_eq!(plain.revision, 1);
}

#[test]
fn state_diff_alignment_request_sends_the_next_cut_as_a_marked_full_state() {
    let template = template_summary();
    let a = summary(&template, "a");
    let senders = diff_senders();
    let mut receiver = senders.stream_events.subscribe();
    let mut producer = StateDiffProducer::default();
    forward_cut(&mut producer, &senders, cut(1, &[&a]));
    forward_cut(&mut producer, &senders, cut(2, &[&a]));
    assert_eq!(kinds(&emitted(&mut receiver)), ["state", "stateDiff"]);

    request_state_diff_base(&senders);
    request_state_diff_base(&senders);
    forward_cut(&mut producer, &senders, cut(3, &[&a]));
    let events = emitted(&mut receiver);
    assert_eq!(kinds(&events), ["state"], "one request, however often made");
    assert_eq!(events[0].1["stateDiffBase"], true);
    assert_eq!(producer.satisfied_generation, 2);

    // The marked cut is the new base, and nothing more is full unasked.
    forward_cut(&mut producer, &senders, cut(4, &[&a]));
    forward_cut(&mut producer, &senders, cut(5, &[&a]));
    let events = emitted(&mut receiver);
    assert_eq!(kinds(&events), ["stateDiff", "stateDiff"]);
    assert_eq!(events[0].1["baseRevision"], 3);
    assert_eq!(events[1].1["baseRevision"], 4);
}

#[test]
fn state_diff_counters_record_each_emission_by_reason() {
    let template = template_summary();
    let a = summary(&template, "a");
    let senders = diff_senders();
    let receiver = senders.stream_events.subscribe();
    let mut producer = StateDiffProducer::default();
    forward_cut(&mut producer, &senders, cut(1, &[&a])); // no base
    forward_cut(&mut producer, &senders, cut(2, &[&a])); // diff
    request_state_diff_base(&senders);
    forward_cut(&mut producer, &senders, cut(3, &[&a])); // alignment
    producer.forward(StateBroadcastWork::Lagged, &senders);
    forward_cut(&mut producer, &senders, cut(4, &[&a])); // lag
    let mut replacement = cut(5, &[&a]);
    replacement.header.server_instance_id = "replacement-instance".to_owned();
    forward_cut(&mut producer, &senders, replacement); // new instance
    drop(receiver);
    let mut lost = cut(6, &[&a]);
    lost.header.server_instance_id = "replacement-instance".to_owned();
    forward_cut(&mut producer, &senders, lost); // a diff nobody received
    let mut receiver = senders.stream_events.subscribe();
    let mut recovered = cut(7, &[&a]);
    recovered.header.server_instance_id = "replacement-instance".to_owned();
    forward_cut(&mut producer, &senders, recovered); // no receivers, not a failure
    let legacy = LegacyStateSubscriber::register(&senders);
    let mut paired = cut(8, &[&a]);
    paired.header.server_instance_id = "replacement-instance".to_owned();
    forward_cut(&mut producer, &senders, paired); // diff + legacy full
    drop(legacy);
    assert_eq!(
        kinds(&emitted(&mut receiver)),
        ["state", "stateDiff", "legacyState"]
    );

    assert_eq!(
        serde_json::to_value(senders.diff_counters.snapshot()).unwrap(),
        json!({
            "diffsEmitted": 2,
            "markedFullStates": {
                "noBase": 1,
                "lag": 1,
                "failure": 0,
                "noReceivers": 1,
                "alignmentRequest": 1,
                "newInstance": 1,
            },
            "legacyPairedFullStates": 1,
            "cutRequests": 0,
        })
    );
}

// Publication without a revision bump: a second cut at the base's revision
// must not become a base a client would never reach (it ignores T <= MB).
// Its changes and the keys touched before it ride the next diff instead.
#[test]
fn state_diff_same_revision_cut_keeps_the_base_and_its_changes_ride_the_next_diff() {
    let template = template_summary();
    let (a, b) = (summary(&template, "a"), summary(&template, "b"));
    let senders = diff_senders();
    let mut receiver = senders.stream_events.subscribe();
    let mut producer = StateDiffProducer::default();
    forward_cut(&mut producer, &senders, cut(1, &[&a, &b]));
    forward_cut(&mut producer, &senders, cut(2, &[&a, &b]));
    assert_eq!(kinds(&emitted(&mut receiver)), ["state", "stateDiff"]);

    producer.forward(
        StateBroadcastWork::Delta(DeltaEvent::ConversationMarkerDeleted {
            revision: 2,
            session_id: "a".to_owned(),
            marker_id: "marker".to_owned(),
            session_mutation_stamp: None,
        }),
        &senders,
    );
    let b_released = Arc::new(StateSessionSummary {
        name: "Released without a bump".to_owned(),
        ..(*b).clone()
    });
    let legacy = LegacyStateSubscriber::register(&senders);
    forward_cut(&mut producer, &senders, cut(2, &[&a, &b_released]));
    drop(legacy);
    // Legacy connections still get the full state; diff clients get nothing.
    assert_eq!(kinds(&emitted(&mut receiver)), ["delta", "legacyState"]);

    forward_cut(&mut producer, &senders, cut(3, &[&a, &b_released]));
    let diff = only_diff(&mut receiver);
    assert_eq!(diff["baseRevision"], 2);
    assert_eq!(diff["revision"], 3);
    let mut changed = ids(&diff["sessions"]["changed"], "id");
    changed.sort();
    assert_eq!(changed, ["a", "b"], "the released change and the touched key both ride");
    let released = diff["sessions"]["changed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "b")
        .unwrap();
    assert_eq!(released["name"], "Released without a bump");
    assert_eq!(
        senders.diff_counters.snapshot().diffs_emitted,
        2,
        "no diff for the same-revision cut"
    );
}

// An alignment request still answers on a same-revision cut, the way the
// full-state route publishes on an idle host.
#[test]
fn state_diff_alignment_on_a_same_revision_cut_sends_the_marked_full_state() {
    let template = template_summary();
    let a = summary(&template, "a");
    let senders = diff_senders();
    let mut receiver = senders.stream_events.subscribe();
    let mut producer = StateDiffProducer::default();
    forward_cut(&mut producer, &senders, cut(4, &[&a]));
    emitted(&mut receiver);
    request_state_diff_base(&senders);
    forward_cut(&mut producer, &senders, cut(4, &[&a]));
    let events = emitted(&mut receiver);
    assert_eq!(kinds(&events), ["state"]);
    assert_eq!(events[0].1["stateDiffBase"], true);
    assert_eq!(events[0].1["revision"], 4);
}

// A cut the broadcaster already dequeued before a full-state request carries
// the older request generation. Processed after the request, it cannot
// answer it; the cut the request published does, even though it shares the
// in-flight cut's revision and reflects a change made without a bump.
#[test]
fn state_diff_in_flight_cut_captured_before_a_request_does_not_answer_it() {
    let (state, session, mailbox) = cached_state();
    let senders = state.state_broadcast_senders.clone();
    let mut receiver = state.subscribe_stream_events();
    let mut producer = StateDiffProducer::default();
    metadata_commit(&state);
    for work in mailbox.take_pending_for_test() {
        producer.forward(work, &senders);
    }
    assert_eq!(kinds(&emitted(&mut receiver)), ["state"]);

    let revision = metadata_commit(&state);
    let mut pending = mailbox.take_pending_for_test();
    assert_eq!(pending.len(), 1);
    // The worker holds this cut but has not processed it yet.
    let in_flight = pending.pop().unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        // A publication without a revision bump changes the session...
        inner.sessions[0].session.name = "Changed without a bump".to_owned();
        // ...and then the full-state route runs: request, then capture.
        request_state_diff_base(&senders);
        state.publish_state_locked(&inner);
    }
    assert_eq!(state.inner.lock().unwrap().revision, revision);

    producer.forward(in_flight, &senders);
    for work in mailbox.take_pending_for_test() {
        producer.forward(work, &senders);
    }
    let events = emitted(&mut receiver);
    assert_eq!(kinds(&events), ["stateDiff", "state"], "the old cut stays a diff");
    assert!(events[0].1.get("stateDiffBase").is_none());
    assert_eq!(events[1].1["stateDiffBase"], true);
    assert_eq!(events[1].1["revision"].as_u64(), Some(revision));
    assert_eq!(events[1].1["sessions"][0]["id"], session);
    assert_eq!(events[1].1["sessions"][0]["name"], "Changed without a bump");
    assert_eq!(
        senders.diff_counters.snapshot().marked_full_states.alignment_request,
        1
    );

    // Answered once: a later cut at that generation is not full again.
    {
        let inner = state.inner.lock().unwrap();
        state.publish_state_locked(&inner);
    }
    for work in mailbox.take_pending_for_test() {
        producer.forward(work, &senders);
    }
    assert!(emitted(&mut receiver).is_empty(), "same revision, already answered");
}

// The requested cut and a later commit's cut coalesce in the mailbox; the
// survivor was captured after the request too, so it answers it once.
#[test]
fn state_diff_request_cut_coalesced_with_a_commit_cut_is_one_marked_full_state() {
    let (state, _, mailbox) = cached_state();
    let senders = state.state_broadcast_senders.clone();
    let mut receiver = state.subscribe_stream_events();
    let mut producer = StateDiffProducer::default();
    metadata_commit(&state);
    for work in mailbox.take_pending_for_test() {
        producer.forward(work, &senders);
    }
    assert_eq!(kinds(&emitted(&mut receiver)), ["state"]);

    {
        let inner = state.inner.lock().unwrap();
        request_state_diff_base(&senders);
        state.publish_state_locked(&inner);
    }
    let committed = metadata_commit(&state);
    let pending = mailbox.take_pending_for_test();
    assert_eq!(pending.len(), 1, "adjacent cuts coalesce");
    for work in pending {
        producer.forward(work, &senders);
    }
    let events = emitted(&mut receiver);
    assert_eq!(kinds(&events), ["state"]);
    assert_eq!(events[0].1["stateDiffBase"], true);
    assert_eq!(events[0].1["revision"].as_u64(), Some(committed));
    let generation = senders.full_state_request_generation.load(Ordering::SeqCst);
    assert_eq!(generation, 1);
    assert_eq!(producer.satisfied_generation, generation);
    assert_eq!(
        senders.diff_counters.snapshot().marked_full_states.alignment_request,
        1
    );
}

// Closing never discards the base: a cut still queued when shutdown begins is
// drained as a diff from it, not as a marked full state.
#[test]
fn state_diff_shutdown_drains_a_pending_cut_as_a_diff_from_the_base() {
    let (mut state, _, mailbox) = cached_state();
    let mut stream = state.subscribe_stream_events();
    let (entered, forwarding) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    *mailbox.before_forward.lock().unwrap() = Some(Box::new(move || {
        entered.send(()).unwrap();
        released.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
    }));
    state.state_broadcast_worker = Some(Arc::new(StateBroadcastWorker::start(state.clone())));
    // The worker holds the first cut (it becomes the base) in the hook.
    let first = metadata_commit(&state);
    forwarding.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
    // A second cut is queued, then shutdown begins with it still pending.
    let second = metadata_commit(&state);
    mailbox.begin_shutdown();
    release.send(()).unwrap();
    state.shutdown_state_broadcast_blocking();
    assert!(mailbox.is_stopped());
    let events = emitted(&mut stream);
    assert_eq!(kinds(&events), ["state", "stateDiff"]);
    assert_eq!(events[0].1["stateDiffBase"], true);
    assert_eq!(events[0].1["revision"].as_u64(), Some(first));
    assert_eq!(events[1].1["baseRevision"].as_u64(), Some(first));
    assert_eq!(events[1].1["revision"].as_u64(), Some(second));
}
