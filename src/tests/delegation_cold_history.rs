//! Finished delegation children keep no resident transcript: boot reads no
//! tail for them, persistence trims them to nothing once their delegation is
//! terminal, and tail, history and result-output reads serve their whole
//! transcript from SQLite. Roots, children of live delegations, parents of
//! live delegations and dangling links keep their resident tail.

use super::acceptance_evaluation::evaluation_target;
use super::delegation_support::{
    finish_delegation_child_with_assistant_text, mark_delegation_as_unstructured_explorer,
    test_app_state_with_drained_delegation_codex_runtime,
};
use super::*;

const TRANSCRIPT_LEN: usize = 130;

fn delegation(id: &str, parent: &str, child: &str, status: DelegationStatus) -> DelegationRecord {
    DelegationRecord {
        original_project_id: None,
        project_basis_recorded: false,
        id: id.to_owned(),
        parent_session_id: parent.to_owned(),
        child_session_id: child.to_owned(),
        mode: DelegationMode::Reviewer,
        status,
        title: "Cold history delegation".to_owned(),
        prompt: "/review-code".to_owned(),
        cwd: "/tmp".to_owned(),
        agent: Agent::Codex,
        model: None,
        write_policy: DelegationWritePolicy::ReadOnly,
        created_at: stamp_now(),
        started_at: Some(stamp_now()),
        completed_at: None,
        result: None,
        submitted_review_result: None,
        post_submission_transport_error: None,
        base_revision: None,
        review_result_recovery_probe_attempt: None,
        review_result_recovery_error: None,
        review_result_schema_version: None,
        queued_followup_prompt_id: None,
        review_result_submission_attempt: 0,
        acceptance_evaluation: None,
        attempt: DelegationAttemptState::default(),
    }
}

fn transcript(id: &str) -> Vec<Message> {
    (0..TRANSCRIPT_LEN)
        .map(|index| Message::Text {
            attachments: Vec::new(),
            id: format!("{id}-m{index:03}"),
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: format!("{id} body {index}"),
            expanded_text: None,
            source: None,
        })
        .collect()
}

fn seed_session(
    inner: &mut StateInner,
    workdir: &str,
    status: SessionStatus,
    parent_delegation_id: Option<&str>,
) -> String {
    let id = inner
        .create_session(Agent::Codex, None, workdir.to_owned(), None, None)
        .session
        .id;
    let index = inner.find_session_index(&id).unwrap();
    let record = inner.session_mut_by_index(index).unwrap();
    replace_session_messages_on_record(record, transcript(&id), None);
    record.session.status = status;
    record.session.parent_delegation_id = parent_delegation_id.map(str::to_owned);
    id
}

/// One root; a completed and a failed delegation child; a child whose
/// delegation row is missing; an idle child of a live delegation; an active
/// child of a live delegation; a completed child that is itself the idle
/// parent of a live delegation, with that child; and a completed child that
/// is the parent of a completed delegation, with that child. The finished
/// children are the completed, failed and both completed nested ones.
struct Kinds {
    root: String,
    completed: String,
    failed: String,
    dangling: String,
    live_idle: String,
    active: String,
    nested_parent: String,
    nested_child: String,
    done_parent: String,
    done_child: String,
}

impl Kinds {
    fn all(&self) -> [&String; 10] {
        [
            &self.root,
            &self.completed,
            &self.failed,
            &self.dangling,
            &self.live_idle,
            &self.active,
            &self.nested_parent,
            &self.nested_child,
            &self.done_parent,
            &self.done_child,
        ]
    }

    fn finished(&self) -> [&String; 4] {
        [
            &self.completed,
            &self.failed,
            &self.done_parent,
            &self.done_child,
        ]
    }
}

fn seed_kinds(inner: &mut StateInner, workdir: &str) -> Kinds {
    let root = seed_session(inner, workdir, SessionStatus::Idle, None);
    let completed = seed_session(inner, workdir, SessionStatus::Idle, Some("d-completed"));
    let failed = seed_session(inner, workdir, SessionStatus::Error, Some("d-failed"));
    let dangling = seed_session(inner, workdir, SessionStatus::Idle, Some("d-missing"));
    let live_idle = seed_session(inner, workdir, SessionStatus::Idle, Some("d-live"));
    let active = seed_session(inner, workdir, SessionStatus::Active, Some("d-active"));
    let nested_parent = seed_session(inner, workdir, SessionStatus::Idle, Some("d-nested-own"));
    let nested_child = seed_session(inner, workdir, SessionStatus::Idle, Some("d-nested"));
    let done_parent = seed_session(inner, workdir, SessionStatus::Idle, Some("d-done-own"));
    let done_child = seed_session(inner, workdir, SessionStatus::Idle, Some("d-done"));
    for (id, parent, child, status) in [
        (
            "d-completed",
            &root,
            &completed,
            DelegationStatus::Completed,
        ),
        ("d-failed", &root, &failed, DelegationStatus::Failed),
        ("d-live", &root, &live_idle, DelegationStatus::Running),
        ("d-active", &root, &active, DelegationStatus::Running),
        (
            "d-nested-own",
            &root,
            &nested_parent,
            DelegationStatus::Completed,
        ),
        (
            "d-nested",
            &nested_parent,
            &nested_child,
            DelegationStatus::Running,
        ),
        (
            "d-done-own",
            &root,
            &done_parent,
            DelegationStatus::Completed,
        ),
        (
            "d-done",
            &done_parent,
            &done_child,
            DelegationStatus::Completed,
        ),
    ] {
        inner
            .delegations
            .push(delegation(id, parent, child, status));
    }
    inner.finish_body_sequence_commits();
    Kinds {
        root,
        completed,
        failed,
        dangling,
        live_idle,
        active,
        nested_parent,
        nested_child,
        done_parent,
        done_child,
    }
}

fn record<'a>(inner: &'a StateInner, id: &str) -> &'a SessionRecord {
    &inner.sessions[inner.find_session_index(id).unwrap()]
}

fn assert_resident_tail(record: &SessionRecord) {
    let start = TRANSCRIPT_LEN - SQLITE_SESSION_TAIL_MESSAGES;
    assert_eq!(
        record.session.message_count as usize, TRANSCRIPT_LEN,
        "session `{}` reports its whole transcript",
        record.session.id
    );
    assert_eq!(
        record.message_start_index, start,
        "session `{}` keeps its resident tail",
        record.session.id
    );
    assert_eq!(record.session.messages.len(), SQLITE_SESSION_TAIL_MESSAGES);
    assert_eq!(
        record.session.messages[0].id(),
        format!("{}-m{start:03}", record.session.id)
    );
}

fn assert_cold(record: &SessionRecord) {
    assert_eq!(
        record.session.message_count as usize, TRANSCRIPT_LEN,
        "a cold session still reports its whole transcript"
    );
    assert!(
        record.session.messages.is_empty(),
        "session `{}` keeps {} resident messages",
        record.session.id,
        record.session.messages.len()
    );
    assert_eq!(record.message_start_index, TRANSCRIPT_LEN);
    assert!(!record.session.messages_loaded);
}

/// Persists the fixture and trims every session as the persistence worker
/// does after a durable commit.
fn persist_and_trim(state: &AppState) -> u64 {
    let mut inner = state.inner.lock().unwrap();
    persist_state(state.persistence_path.as_ref(), &inner).unwrap();
    let watermark = inner.last_mutation_stamp;
    let ids = inner
        .sessions
        .iter()
        .map(|record| record.session.id.clone())
        .collect::<Vec<_>>();
    inner.trim_persisted_session_tails(watermark, &ids);
    watermark
}

/// One pass of the persistence worker: collect the changes since
/// `watermark`, commit them, then trim only the sessions that pass wrote.
/// Returns the new watermark and the sessions it persisted.
fn persistence_pass(state: &AppState, watermark: u64) -> (u64, Vec<String>) {
    let delta = collect_persist_delta_from_shared_state(&state.inner, watermark);
    let persisted = persist_delta_via_cache(
        &mut SqlitePersistConnectionCache::new(),
        state.persistence_path.as_path(),
        &delta,
    )
    .unwrap();
    trim_persisted_session_tails_from_shared_state(&state.inner, delta.watermark, &persisted);
    trim_cold_transcript_candidates_from_shared_state(&state.inner, delta.watermark);
    (delta.watermark, persisted)
}

fn rename(state: &AppState, session_id: &str, name: &str) {
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(session_id).unwrap();
    inner.session_mut_by_index(index).unwrap().session.name = name.to_owned();
}

fn seeded_state() -> (AppState, Kinds) {
    let state = test_app_state();
    let workdir = test_temp_dir().to_string_lossy().into_owned();
    let kinds = seed_kinds(&mut state.inner.lock().unwrap(), &workdir);
    (state, kinds)
}

fn ids_of(messages: impl IntoIterator<Item = Message>) -> Vec<String> {
    messages
        .into_iter()
        .map(|message| message.id().to_owned())
        .collect()
}

fn selector(
    before: Option<&str>,
    start: Option<usize>,
    around: Option<usize>,
    limit: usize,
) -> SessionHistorySelector<'_> {
    SessionHistorySelector {
        before,
        after: None,
        around,
        start,
        from_start: false,
        limit,
    }
}

fn expected(id: &str, range: std::ops::Range<usize>) -> Vec<String> {
    range.map(|index| format!("{id}-m{index:03}")).collect()
}

/// Whole serialized messages, so a read is compared body for body.
fn bodies(messages: impl IntoIterator<Item = Message>) -> Vec<serde_json::Value> {
    messages
        .into_iter()
        .map(|message| serde_json::to_value(message).unwrap())
        .collect()
}

/// Every seeded session's whole transcript, captured while still resident.
fn seeded_bodies(state: &AppState, kinds: &Kinds) -> BTreeMap<String, Vec<serde_json::Value>> {
    let inner = state.inner.lock().unwrap();
    kinds
        .all()
        .into_iter()
        .map(|id| {
            let record = record(&inner, id);
            assert_eq!(record.session.messages.len(), TRANSCRIPT_LEN);
            (id.clone(), bodies(record.session.messages.iter().cloned()))
        })
        .collect()
}

#[test]
fn boot_reads_no_transcript_tail_for_a_finished_delegation_child() {
    let root = TestTempRoot::create("cold-history-boot");
    let path = root.join("termal.sqlite");
    let workdir = root.to_string_lossy().into_owned();
    let mut inner = StateInner::new();
    let kinds = seed_kinds(&mut inner, &workdir);
    persist_state(&path, &inner).expect("the fixture persists");

    // Production boot defers delegation rows. Each tail load is one
    // TranscriptTails call, so the count is the number of tails read.
    let mut timings = BootStateLoadTimings::new(true);
    BOOT_TAIL_LOADS.with(|loads| loads.borrow_mut().clear());
    let (loaded, _connection) =
        load_state_from_sqlite_with_timings(&path, true, &mut timings).expect("the fixture boots");
    let loaded = loaded.unwrap();
    // Per session: exactly the sessions that are not finished children had a
    // transcript tail read, each once.
    let mut tail_loads = BOOT_TAIL_LOADS.with(|loads| loads.borrow_mut().split_off(0));
    tail_loads.sort();
    let mut want = kinds
        .all()
        .into_iter()
        .filter(|id| !kinds.finished().contains(id))
        .cloned()
        .collect::<Vec<_>>();
    want.sort();
    assert_eq!(tail_loads, want);
    let calls = |phase| {
        timings
            .measurements
            .get(&phase)
            .map(|measurement| measurement.calls)
    };
    assert_eq!(
        calls(BootStateLoadPhase::TranscriptTails),
        Some(kinds.all().len() - kinds.finished().len()),
        "every session except the finished children reads its tail"
    );
    for id in kinds.finished() {
        assert_cold(record(&loaded, id));
    }
    // Recovery of a live delegation, the nested parent's card and the
    // existing repair of a dangling link all find the same resident tail.
    for id in [
        &kinds.root,
        &kinds.dangling,
        &kinds.live_idle,
        &kinds.nested_parent,
        &kinds.nested_child,
    ] {
        assert_resident_tail(record(&loaded, id));
    }
    // Boot recovers the interrupted active child onto its loaded tail by
    // appending one restart notice after the 64 resident messages.
    let active = record(&loaded, &kinds.active);
    let start = TRANSCRIPT_LEN - SQLITE_SESSION_TAIL_MESSAGES;
    assert_eq!(active.session.message_count as usize, TRANSCRIPT_LEN + 1);
    assert_eq!(active.message_start_index, start);
    assert_eq!(
        active.session.messages.len(),
        SQLITE_SESSION_TAIL_MESSAGES + 1
    );
    assert_eq!(
        active.session.messages[0].id(),
        format!("{}-m{start:03}", kinds.active)
    );
}

#[test]
fn a_malformed_delegation_row_leaves_the_finished_children_cold_at_boot() {
    let root = TestTempRoot::create("cold-history-malformed-link");
    let path = root.join("termal.sqlite");
    let workdir = root.to_string_lossy().into_owned();
    let mut inner = StateInner::new();
    let kinds = seed_kinds(&mut inner, &workdir);
    persist_state(&path, &inner).expect("the fixture persists");
    // The deferred inventory quarantines this row; the link prepass must not
    // fall back to loading every tail because of it.
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "INSERT INTO delegations(id, value_json) VALUES('delegation-malformed', '{')",
            [],
        )
        .unwrap();
    let mut timings = BootStateLoadTimings::new(true);
    let (loaded, _connection) =
        load_state_from_sqlite_with_timings(&path, true, &mut timings).expect("the fixture boots");
    let loaded = loaded.unwrap();
    for id in kinds.finished() {
        assert_cold(record(&loaded, id));
    }
    assert_resident_tail(record(&loaded, &kinds.root));
}

#[test]
fn persistence_trims_a_finished_delegation_child_to_no_resident_messages() {
    let (state, kinds) = seeded_state();
    persist_and_trim(&state);
    let inner = state.inner.lock().unwrap();
    for id in kinds.finished() {
        assert_cold(record(&inner, id));
    }
    for id in [
        &kinds.root,
        &kinds.dangling,
        &kinds.live_idle,
        &kinds.nested_parent,
        &kinds.nested_child,
    ] {
        assert_resident_tail(record(&inner, id));
    }
    assert_eq!(
        record(&inner, &kinds.active).session.messages.len(),
        TRANSCRIPT_LEN,
        "an active session is never trimmed"
    );
}

#[test]
fn an_open_test_run_card_keeps_a_finished_child_resident() {
    let (state, kinds) = seeded_state();
    {
        let mut inner = state.inner.lock().unwrap();
        let card = TestRunCardRef {
            session_id: kinds.completed.clone(),
            message_id: format!("{}-m129", kinds.completed),
            terminal: false,
            failure_detail_version: None,
        };
        inner
            .test_run_cards
            .insert("run-still-open".to_owned(), card);
    }
    persist_and_trim(&state);
    let inner = state.inner.lock().unwrap();
    assert_resident_tail(record(&inner, &kinds.completed));
    assert_cold(record(&inner, &kinds.failed));
}

#[test]
fn a_stored_card_of_a_live_run_keeps_its_owner_resident_at_boot_and_a_terminal_one_does_not() {
    let root = TestTempRoot::create("cold-history-card-boot");
    let path = root.join("termal.sqlite");
    let workdir = root.to_string_lossy().into_owned();
    let mut inner = StateInner::new();
    let kinds = seed_kinds(&mut inner, &workdir);
    // The failed child owns a card of a run that was still live: the first
    // scan after a restart may update it from the owner's transcript, so the
    // child has something running. The completed child's only card is of a
    // terminal run.
    inner.test_run_cards.insert(
        "run-live-before-restart".to_owned(),
        TestRunCardRef {
            session_id: kinds.failed.clone(),
            message_id: format!("{}-m129", kinds.failed),
            terminal: false,
            failure_detail_version: None,
        },
    );
    inner.test_run_cards.insert(
        "run-done-before-restart".to_owned(),
        TestRunCardRef {
            session_id: kinds.completed.clone(),
            message_id: format!("{}-m129", kinds.completed),
            terminal: true,
            failure_detail_version: None,
        },
    );
    persist_state(&path, &inner).expect("the fixture persists");
    BOOT_TAIL_LOADS.with(|loads| loads.borrow_mut().clear());
    let (loaded, _connection) =
        load_state_from_sqlite_with_timings(&path, true, &mut BootStateLoadTimings::new(false))
            .expect("the fixture boots");
    let mut loaded = loaded.unwrap();
    let tail_loads = BOOT_TAIL_LOADS.with(|loads| loads.borrow_mut().split_off(0));
    assert!(tail_loads.contains(&kinds.failed));
    assert!(!tail_loads.contains(&kinds.completed));
    assert_resident_tail(record(&loaded, &kinds.failed));
    assert_cold(record(&loaded, &kinds.completed));
    // The cold owner's card keeps its stored terminal flag, since its
    // transcript is not scanned; the live one is re-derived as before.
    assert!(loaded.test_run_cards["run-done-before-restart"].terminal);
    assert!(!loaded.test_run_cards["run-live-before-restart"].terminal);
    // Removing the live card later mutates no session, so the boot-held child
    // is queued for the per-pass recheck instead: it stays resident while the
    // card is indexed and the delegations are loading, then goes cold.
    assert!(loaded.cold_transcripts.is_candidate(&kinds.failed));
    let watermark = loaded.last_mutation_stamp;
    drop(loaded.trim_cold_transcript_candidates(watermark));
    assert_resident_tail(record(&loaded, &kinds.failed));
    loaded.delegations = inner.delegations.clone();
    loaded.delegation_inventory = DelegationInventoryState::Ready;
    drop(loaded.trim_cold_transcript_candidates(watermark));
    assert_resident_tail(record(&loaded, &kinds.failed));
    loaded.test_run_cards.remove("run-live-before-restart");
    drop(loaded.trim_cold_transcript_candidates(watermark));
    assert_cold(record(&loaded, &kinds.failed));
    assert!(!loaded.cold_transcripts.is_candidate(&kinds.failed));
}

#[test]
fn a_pending_late_file_change_capture_keeps_a_finished_child_resident() {
    let (state, kinds) = seeded_state();
    let capture = {
        let mut inner = state.inner.lock().unwrap();
        let completed = inner.find_session_index(&kinds.completed).unwrap();
        let tracking = Arc::new(FileChangeTrackingLifetime::new(None));
        inner.sessions[completed].file_change_tracking = Some(Arc::clone(&tracking));
        let failed = inner.find_session_index(&kinds.failed).unwrap();
        // A grace window that is still open: its deadline lies ahead.
        inner.sessions[failed].active_turn_file_change_grace_deadline =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        // An in-flight capture holds the tracking until it is applied.
        tracking
    };
    persist_and_trim(&state);
    {
        let inner = state.inner.lock().unwrap();
        assert_resident_tail(record(&inner, &kinds.completed));
        assert_resident_tail(record(&inner, &kinds.failed));
    }
    drop(capture);
    {
        let mut inner = state.inner.lock().unwrap();
        let failed = inner.find_session_index(&kinds.failed).unwrap();
        inner.sessions[failed].active_turn_file_change_grace_deadline = None;
    }
    persist_and_trim(&state);
    let inner = state.inner.lock().unwrap();
    for id in kinds.finished() {
        assert_cold(record(&inner, id));
    }
}

#[test]
fn tail_and_history_reads_return_the_whole_transcript_for_every_session_kind() {
    let (state, kinds) = seeded_state();
    let originals = seeded_bodies(&state, &kinds);
    persist_and_trim(&state);
    for id in kinds.all() {
        for limit in [1, 20, SQLITE_SESSION_TAIL_MESSAGES] {
            let tail = state.get_session_tail(id, limit).unwrap();
            assert_eq!(tail.session.message_count as usize, TRANSCRIPT_LEN);
            assert_eq!(
                ids_of(tail.session.messages.iter().cloned()),
                expected(id, TRANSCRIPT_LEN - limit..TRANSCRIPT_LEN),
                "tail {limit} of `{id}`"
            );
            assert_eq!(
                bodies(tail.session.messages.iter().cloned()),
                originals[id][TRANSCRIPT_LEN - limit..].to_vec(),
                "tail {limit} bodies of `{id}`"
            );
        }
        let before = format!("{id}-m050");
        for (query, range) in [
            (selector(None, Some(0), None, 40), 0..40),
            (selector(None, Some(50), None, 40), 50..90),
            (selector(Some(&before), None, None, 10), 40..50),
            (selector(None, None, Some(100), 20), 90..110),
        ] {
            let page = state
                .capture_local_session_history(id, query)
                .unwrap()
                .page(id, query, state.server_instance_id.clone())
                .unwrap();
            assert_eq!(page.message_start_index, range.start, "history of `{id}`");
            assert_eq!(
                bodies(page.messages.iter().cloned()),
                originals[id][range.clone()].to_vec(),
                "history bodies of `{id}`"
            );
            assert_eq!(ids_of(page.messages), expected(id, range));
        }
    }
    // Reading never made a cold transcript resident again.
    let inner = state.inner.lock().unwrap();
    for id in kinds.finished() {
        assert_cold(record(&inner, id));
    }
}

#[test]
fn a_cold_childs_window_is_installed_only_for_the_version_it_was_read_from() {
    let (state, kinds) = seeded_state();
    persist_and_trim(&state);

    let stale = state
        .read_cold_session_window(&kinds.failed)
        .unwrap()
        .expect("a cold child reads its window from SQLite");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&kinds.failed).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.session.name = "renamed after the read".to_owned();
        assert!(install_cold_session_window(record, stale).is_err());
        assert_cold(&inner.sessions[index]);
    }

    drop(state.hydrate_cold_session_window(&kinds.completed).unwrap());
    let inner = state.inner.lock().unwrap();
    let hydrated = record(&inner, &kinds.completed);
    assert_resident_tail(hydrated);
    assert_eq!(
        hydrated.message_positions,
        build_message_positions(&hydrated.session.messages)
    );
    assert!(
        !inner.cold_transcripts.has_pins(),
        "a dropped pin is released"
    );
}

#[test]
fn a_finished_child_goes_cold_after_its_file_change_grace_window_ends() {
    let (state, kinds) = seeded_state();
    let watermark = persist_and_trim(&state);
    let child = kinds.completed.clone();
    // A turn of the finished child ends: its last message lands and the
    // post-turn file-change grace window opens, in the same mutation.
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&child).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.active_turn_start_message_count = Some(TRANSCRIPT_LEN);
        push_message_on_record(
            record,
            Message::Text {
                attachments: Vec::new(),
                id: format!("{child}-turn-end"),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "turn end".to_owned(),
                expanded_text: None,
                source: None,
            },
        );
        finish_active_turn_file_change_tracking(record);
        // Still open whenever the pass below runs.
        assert!(record.active_turn_file_change_grace_deadline.is_some());
        record.active_turn_file_change_grace_deadline =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(3600));
    }
    let (watermark, persisted) = persistence_pass(&state, watermark);
    assert!(persisted.contains(&child));
    {
        let inner = state.inner.lock().unwrap();
        let record = record(&inner, &child);
        assert_eq!(
            record.session.messages.len(),
            1,
            "an open grace window keeps the new message resident"
        );
        assert!(inner.cold_transcripts.is_candidate(&child));
    }
    // Nothing may be written after the window: the pass's recheck returns
    // just after its end, and the persistence worker's wait is armed with it.
    let deadline = record(&state.inner.lock().unwrap(), &child)
        .active_turn_file_change_grace_deadline
        .unwrap();
    let recheck_at = trim_cold_transcript_candidates_from_shared_state(&state.inner, watermark);
    assert_eq!(
        recheck_at,
        Some(deadline + std::time::Duration::from_millis(1))
    );
    let armed = PersistWorkerRetryState {
        cold_transcript_recheck_at: recheck_at,
        ..PersistWorkerRetryState::default()
    };
    let delay = armed.next_tick_delay().expect("the wait is timed");
    assert!(delay > std::time::Duration::from_secs(3500));
    assert!(delay <= std::time::Duration::from_secs(3601));
    // The window has ended (a deadline in the past). Nothing persists the
    // child again; the worker's own wake rechecks the candidate in a pass
    // that writes no session at all.
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&child).unwrap();
        inner.sessions[index].active_turn_file_change_grace_deadline =
            Some(std::time::Instant::now() - std::time::Duration::from_millis(1));
    }
    let (_, persisted) = persistence_pass(&state, watermark);
    assert!(persisted.is_empty());
    let inner = state.inner.lock().unwrap();
    let record = record(&inner, &child);
    assert!(record.session.messages.is_empty());
    assert_eq!(record.message_start_index, TRANSCRIPT_LEN + 1);
    assert_eq!(record.session.message_count as usize, TRANSCRIPT_LEN + 1);
    assert!(!inner.cold_transcripts.is_candidate(&child));
    drop(inner);
    assert_eq!(
        trim_cold_transcript_candidates_from_shared_state(&state.inner, watermark),
        None
    );
}

#[test]
fn the_persistence_worker_wakes_for_a_grace_recheck_with_no_write_queued() {
    let (_persist_tx, persist_rx) = std::sync::mpsc::channel::<PersistRequest>();
    let due = PersistWorkerRetryState {
        cold_transcript_recheck_at: Some(std::time::Instant::now()),
        ..PersistWorkerRetryState::default()
    };
    assert_eq!(
        due.wait_for_next_tick(&persist_rx, &mut PersistFenceBatch::default()),
        PersistWorkerWaitOutcome::Process
    );
}

#[test]
fn a_pinned_window_survives_persistence_until_its_pin_drops() {
    let (mut state, kinds) = seeded_state();
    let (persist_tx, persist_rx) = std::sync::mpsc::channel::<PersistRequest>();
    state.persist_tx = persist_tx;
    let watermark = persist_and_trim(&state);
    let pin = state
        .hydrate_cold_session_window(&kinds.completed)
        .expect("a cold child's window is installed");
    // A pass that writes another session leaves the pinned window alone.
    rename(&state, &kinds.root, "unrelated change");
    let (watermark, persisted) = persistence_pass(&state, watermark);
    assert!(!persisted.contains(&kinds.completed));
    {
        let inner = state.inner.lock().unwrap();
        assert_resident_tail(record(&inner, &kinds.completed));
        assert!(inner.cold_transcripts.has_pins());
    }
    // Reading the window changed no stamp, so no later pass writes the
    // child: releasing the pin queues it and wakes the worker for a pass
    // whose recheck runs even though it writes no session at all.
    while persist_rx.try_recv().is_ok() {}
    drop(pin);
    assert!(matches!(persist_rx.try_recv(), Ok(PersistRequest::Delta)));
    {
        let inner = state.inner.lock().unwrap();
        assert!(!inner.cold_transcripts.has_pins());
        assert!(inner.cold_transcripts.is_candidate(&kinds.completed));
    }
    let (_, persisted) = persistence_pass(&state, watermark);
    assert!(persisted.is_empty());
    assert_cold(record(&state.inner.lock().unwrap(), &kinds.completed));
}

#[test]
fn a_test_run_card_reference_keeps_its_owner_resident_until_the_index_drops_it() {
    let (state, kinds) = seeded_state();
    state.inner.lock().unwrap().test_run_cards.insert(
        "run-finished".to_owned(),
        TestRunCardRef {
            session_id: kinds.completed.clone(),
            message_id: format!("{}-m129", kinds.completed),
            terminal: true,
            failure_detail_version: None,
        },
    );
    let watermark = persist_and_trim(&state);
    {
        let inner = state.inner.lock().unwrap();
        assert_resident_tail(record(&inner, &kinds.completed));
        assert!(inner.cold_transcripts.is_candidate(&kinds.completed));
    }
    state
        .inner
        .lock()
        .unwrap()
        .test_run_cards
        .remove("run-finished");
    rename(&state, &kinds.root, "unrelated change");
    persistence_pass(&state, watermark);
    assert_cold(record(&state.inner.lock().unwrap(), &kinds.completed));
}

#[test]
fn a_finished_childs_damaged_row_is_refused_by_its_first_on_demand_read() {
    // The same damage at the same position of a root, whose tail boot loads
    // and checks, and of a finished child, whose rows boot does not read.
    for (case, damage) in [
        (
            "missing row",
            "DELETE FROM messages WHERE session_id = ?1 AND position = 100",
        ),
        (
            "malformed body",
            "UPDATE messages SET value_json = '{' WHERE session_id = ?1 AND position = 100",
        ),
        (
            "row id mismatch",
            "UPDATE messages SET message_id = 'another-id' WHERE session_id = ?1 AND position = 100",
        ),
    ] {
        let root = TestTempRoot::create("cold-history-damage");
        let path = root.join("termal.sqlite");
        let workdir = root.to_string_lossy().into_owned();
        let mut inner = StateInner::new();
        let kinds = seed_kinds(&mut inner, &workdir);
        persist_state(&path, &inner).expect("the fixture persists");
        let connection = rusqlite::Connection::open(&path).unwrap();
        for id in [&kinds.root, &kinds.completed] {
            assert_eq!(connection.execute(damage, [id]).unwrap(), 1, "{case}");
        }
        drop(connection);
        let state = AppState::new_with_paths(
            workdir.clone(),
            path.clone(),
            root.join("orchestrators.json"),
        )
        .expect("damaged sessions do not stop the boot");
        {
            let inner = state.inner.lock().unwrap();
            // The loaded tail is checked at boot and quarantined, as before.
            assert!(
                inner
                    .quarantined_persisted_session_ids
                    .contains(&kinds.root),
                "{case}: the root is quarantined"
            );
            assert!(inner.find_session_index(&kinds.root).is_none(), "{case}");
            // The finished child boots cold without its rows being read.
            assert!(
                !inner
                    .quarantined_persisted_session_ids
                    .contains(&kinds.completed),
                "{case}"
            );
            assert_cold(record(&inner, &kinds.completed));
        }
        // Its first read over the damaged position refuses it, naming it,
        // instead of serving a transcript without that row.
        let error = state
            .get_session_tail(&kinds.completed, SQLITE_SESSION_TAIL_MESSAGES)
            .err()
            .expect(case);
        assert!(
            error.message.contains("position 100"),
            "{case}: {}",
            error.message
        );
        let query = selector(None, Some(90), None, 20);
        let error = state
            .capture_local_session_history(&kinds.completed, query)
            .and_then(|snapshot| {
                snapshot.page(&kinds.completed, query, state.server_instance_id.clone())
            })
            .err()
            .expect(case);
        assert!(
            error.message.contains("position 100"),
            "{case}: {}",
            error.message
        );
        // An undamaged range still reads.
        let query = selector(None, Some(0), None, 40);
        let page = state
            .capture_local_session_history(&kinds.completed, query)
            .unwrap()
            .page(&kinds.completed, query, state.server_instance_id.clone())
            .unwrap();
        assert_eq!(
            ids_of(page.messages),
            expected(&kinds.completed, 0..40),
            "{case}"
        );
        state.shutdown_persist_blocking();
    }
}

#[test]
fn delegation_recovery_never_meets_a_cold_child_of_an_open_link() {
    let (state, kinds) = seeded_state();
    persist_and_trim(&state);
    // The loaded records reopen the completed child's delegation, as a
    // follow-up recorded before a restart would.
    let mut records = state.inner.lock().unwrap().delegations.clone();
    for record in &mut records {
        if record.child_session_id == kinds.completed {
            record.status = DelegationStatus::Running;
        }
    }
    let windows = state.read_windows_for_open_links(&records).unwrap();
    assert_eq!(
        windows
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        [kinds.completed.as_str()]
    );
    let mut inner = state.inner.lock().unwrap();
    for (id, window) in windows {
        let index = inner.find_session_index(&id).unwrap();
        install_cold_session_window(&mut inner.sessions[index], window).unwrap();
    }
    assert_resident_tail(record(&inner, &kinds.completed));
    assert_cold(record(&inner, &kinds.failed));
}

#[test]
fn a_cold_childs_persistence_append_and_reopen_keep_every_position() {
    let (state, kinds) = seeded_state();
    let mut originals = seeded_bodies(&state, &kinds);
    persist_and_trim(&state);
    let path = state.persistence_path.as_path().to_owned();
    let appended = format!("{}-appended", kinds.completed);
    let follow_up = Message::Text {
        attachments: Vec::new(),
        id: appended.clone(),
        timestamp: stamp_now(),
        author: Author::Assistant,
        text: "follow-up".to_owned(),
        expanded_text: None,
        source: None,
    };
    originals
        .get_mut(&kinds.completed)
        .unwrap()
        .push(serde_json::to_value(&follow_up).unwrap());
    {
        let mut inner = state.inner.lock().unwrap();
        // A full snapshot and a metadata-only delta while the child is cold.
        persist_state(&path, &inner).unwrap();
        let watermark = inner.last_mutation_stamp;
        let index = inner.find_session_index(&kinds.completed).unwrap();
        inner.session_mut_by_index(index).unwrap().session.name = "cold rename".to_owned();
        let delta = inner.collect_persist_delta(watermark);
        persist_delta_via_cache(&mut SqlitePersistConnectionCache::new(), &path, &delta).unwrap();
        // A follow-up appends after the durable transcript.
        let watermark = inner.last_mutation_stamp;
        let record = inner.session_mut_by_index(index).unwrap();
        push_message_on_record(record, follow_up);
        let delta = inner.collect_persist_delta(watermark);
        persist_delta_via_cache(&mut SqlitePersistConnectionCache::new(), &path, &delta).unwrap();
    }
    // Every position, id and body is stored, before and after a reopen.
    let assert_stored = |when: &str| {
        for id in kinds.all() {
            let len = if *id == kinds.completed {
                TRANSCRIPT_LEN + 1
            } else {
                TRANSCRIPT_LEN
            };
            let stored = load_persisted_message_range(&path, id, 0, len + 1).unwrap();
            let mut want = expected(id, 0..TRANSCRIPT_LEN);
            if *id == kinds.completed {
                want.push(appended.clone());
            }
            assert_eq!(
                stored
                    .iter()
                    .map(|(position, _)| *position)
                    .collect::<Vec<_>>(),
                (0..len).collect::<Vec<_>>(),
                "positions of `{id}` {when}"
            );
            let messages = stored
                .into_iter()
                .map(|(_, message)| message)
                .collect::<Vec<_>>();
            assert_eq!(
                bodies(messages.iter().cloned()),
                originals[id],
                "bodies of `{id}` {when}"
            );
            assert_eq!(ids_of(messages), want, "ids of `{id}` {when}");
        }
    };
    assert_stored("before the reopen");
    let (reopened, _connection) =
        load_state_from_sqlite_with_timings(&path, true, &mut BootStateLoadTimings::new(false))
            .unwrap();
    let reopened = reopened.unwrap();
    assert_stored("after the reopen");
    let completed = record(&reopened, &kinds.completed);
    assert_eq!(completed.session.message_count as usize, TRANSCRIPT_LEN + 1);
    assert_eq!(completed.session.name, "cold rename");
    assert_cold_after_append(completed);
    // The sessions that reopen with a tail hold the same bodies.
    for id in [
        &kinds.root,
        &kinds.dangling,
        &kinds.live_idle,
        &kinds.nested_parent,
        &kinds.nested_child,
    ] {
        let tail = &record(&reopened, id).session.messages;
        assert_eq!(
            bodies(tail.iter().cloned()),
            originals[id][TRANSCRIPT_LEN - SQLITE_SESSION_TAIL_MESSAGES..].to_vec(),
            "reopened tail of `{id}`"
        );
    }
    // The interrupted active child reopens with the same tail and one restart
    // notice after it.
    let active = &record(&reopened, &kinds.active).session.messages;
    assert_eq!(active.len(), SQLITE_SESSION_TAIL_MESSAGES + 1);
    assert_eq!(
        bodies(active.iter().take(SQLITE_SESSION_TAIL_MESSAGES).cloned()),
        originals[&kinds.active][TRANSCRIPT_LEN - SQLITE_SESSION_TAIL_MESSAGES..].to_vec(),
        "reopened tail of the active child"
    );
}

fn assert_cold_after_append(record: &SessionRecord) {
    assert!(record.session.messages.is_empty());
    assert_eq!(record.message_start_index, TRANSCRIPT_LEN + 1);
}

#[test]
fn a_cold_childs_result_output_pages_equal_its_resident_output() {
    let state = test_app_state_with_drained_delegation_codex_runtime("cold-history-output");
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Return a byte-complete artifact.".to_owned(),
                title: Some("Cold output".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    mark_delegation_as_unstructured_explorer(&state, &created.delegation.id);
    let child_id = created.delegation.child_session_id.clone();
    let output = format!("cold output {}", "0123456789".repeat(900));
    finish_delegation_child_with_assistant_text(&state, &child_id, &output);
    state
        .refresh_delegation_for_child_session(&child_id)
        .expect("refresh should complete");
    let pages = |state: &AppState| {
        let mut reconstructed = String::new();
        let mut offset = 0;
        loop {
            let page = state
                .get_delegation_result_output(
                    &parent_session_id,
                    &created.delegation.id,
                    offset,
                    MIN_DELEGATION_RESULT_OUTPUT_PAGE_BYTES + 1,
                )
                .expect("full-output page should be available");
            assert!(
                !page.summary_fallback,
                "the raw output is served, not the summary"
            );
            reconstructed.push_str(&page.output);
            match page.next_offset_bytes {
                Some(next) => offset = next,
                None => break reconstructed,
            }
        }
    };
    assert_eq!(pages(&state), output);

    {
        let mut inner = state.inner.lock().unwrap();
        persist_state(state.persistence_path.as_ref(), &inner).unwrap();
        let index = inner.find_session_index(&child_id).unwrap();
        drop(detach_retained_session_messages(
            &mut inner.sessions[index],
            0,
        ));
        assert!(inner.sessions[index].session.messages.is_empty());
    }
    assert_eq!(pages(&state), output);

    // A persistence pass trims the child after the read checked its window
    // and before it takes the lock: the read checks again and still pages
    // the raw output.
    drop(state.hydrate_cold_session_window(&child_id).unwrap());
    let trim_state = state.clone();
    let trim_child = child_id.clone();
    TEST_RESULT_OUTPUT_AFTER_WINDOW_CHECK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let mut inner = trim_state.inner.lock().unwrap();
            let index = inner.find_session_index(&trim_child).unwrap();
            drop(detach_retained_session_messages(
                &mut inner.sessions[index],
                0,
            ));
        }));
    });
    assert_eq!(pages(&state), output);

    // After a real restart the child boots cold, and its output pages are
    // still the raw output.
    state.shutdown_persist_blocking();
    let restarted = AppState::new_with_paths(
        state.default_workdir.clone(),
        state.persistence_path.as_ref().clone(),
        state.orchestrator_templates_path.as_ref().clone(),
    )
    .expect("the store boots");
    restarted.shutdown_persist_blocking();
    assert!(
        record(&restarted.inner.lock().unwrap(), &child_id)
            .session
            .messages
            .is_empty()
    );
    assert_eq!(pages(&restarted), output);
}

#[test]
fn a_tail_read_serves_the_transcript_when_a_trim_lands_between_its_locks() {
    let (state, kinds) = seeded_state();
    let watermark = {
        let inner = state.inner.lock().unwrap();
        persist_state(state.persistence_path.as_ref(), &inner).unwrap();
        inner.last_mutation_stamp
    };
    // Fully resident when the read classifies it; trimmed to nothing, as
    // the persistence worker does, before the read projects it.
    let trim_state = state.clone();
    let trim_child = kinds.completed.clone();
    TEST_SESSION_TAIL_BETWEEN_LOCKS.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            trim_state
                .inner
                .lock()
                .unwrap()
                .trim_persisted_session_tails(watermark, &[trim_child]);
        }));
    });
    let tail = state.get_session_tail(&kinds.completed, 20).unwrap();
    assert_cold(record(&state.inner.lock().unwrap(), &kinds.completed));
    assert_eq!(
        ids_of(tail.session.messages.iter().cloned()),
        expected(&kinds.completed, TRANSCRIPT_LEN - 20..TRANSCRIPT_LEN)
    );
}

#[test]
fn a_handoff_prompt_in_a_cold_owners_transcript_counts_as_delivered() {
    let (state, kinds) = seeded_state();
    persist_and_trim(&state);
    assert_cold(record(&state.inner.lock().unwrap(), &kinds.completed));
    // The prompt is in the durable transcript only: delivered, not lost, so
    // no second wake is queued under its id.
    let delivered = format!("{}-m100", kinds.completed);
    assert_eq!(
        state
            .handoff_prompt_delivery(&kinds.completed, &delivered)
            .0,
        HandoffOfferDelivery::Delivered
    );
    assert_eq!(
        state
            .handoff_prompt_delivery(&kinds.completed, "never-queued")
            .0,
        HandoffOfferDelivery::Missing
    );
    // A Codex rollback replaces the transcript in memory before its rows are
    // rewritten: the old row of a prompt the replacement dropped is not a
    // delivery.
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&kinds.completed).unwrap();
        replace_session_messages_on_record(
            inner.session_mut_by_index(index).unwrap(),
            transcript("replacement"),
            None,
        );
    }
    assert_eq!(
        state
            .handoff_prompt_delivery(&kinds.completed, &delivered)
            .0,
        HandoffOfferDelivery::Missing
    );
}

#[test]
fn the_grace_recheck_is_armed_for_the_window_the_trim_kept_open() {
    let (state, kinds) = seeded_state();
    let watermark = persist_and_trim(&state);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3600);
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&kinds.completed).unwrap();
    inner.sessions[index].active_turn_file_change_grace_deadline = Some(deadline);
    inner
        .cold_transcripts
        .enroll_candidates([kinds.completed.clone()]);
    // At the window's last instant the trim keeps the child, and the recheck
    // decided at that same instant is armed for just after it.
    drop(inner.trim_cold_transcript_candidates_at(watermark, deadline));
    assert!(inner.cold_transcripts.is_candidate(&kinds.completed));
    let recheck_at = inner.cold_transcript_grace_recheck_at(deadline);
    assert_eq!(
        recheck_at,
        Some(deadline + std::time::Duration::from_millis(1))
    );
    // That recheck releases the child, and nothing is armed after it.
    let after = recheck_at.unwrap();
    drop(inner.trim_cold_transcript_candidates_at(watermark, after));
    assert!(!inner.cold_transcripts.is_candidate(&kinds.completed));
    assert_eq!(inner.cold_transcript_grace_recheck_at(after), None);
}

#[test]
fn a_watcher_capture_of_a_held_finished_child_wakes_its_recheck() {
    let (mut state, kinds) = seeded_state();
    let (persist_tx, persist_rx) = std::sync::mpsc::channel::<PersistRequest>();
    state.persist_tx = persist_tx;
    persist_and_trim(&state);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&kinds.completed).unwrap();
        inner.sessions[index].active_turn_file_change_grace_deadline =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(3600));
    }
    // A change for another session: the open window makes the child a
    // capture owner, but nothing matches it.
    let unrelated = [WorkspaceFileChangeEvent {
        path: "unrelated.rs".to_owned(),
        kind: WorkspaceFileChangeKind::Created,
        root_path: None,
        session_id: Some("another-session".to_owned()),
        mtime_ms: None,
        size_bytes: None,
    }];
    while persist_rx.try_recv().is_ok() {}
    state.record_active_turn_file_changes(&unrelated);
    assert!(
        persist_rx.try_recv().is_err(),
        "a capture of a session no recheck waits for wakes nothing"
    );
    // The child is held by that capture while a recheck runs; when the
    // capture releases it, matching nothing, the worker wakes for another.
    state
        .inner
        .lock()
        .unwrap()
        .cold_transcripts
        .enroll_candidates([kinds.completed.clone()]);
    state.record_active_turn_file_changes(&unrelated);
    assert!(matches!(persist_rx.try_recv(), Ok(PersistRequest::Delta)));
}

#[test]
fn a_child_whose_delegation_ends_after_boot_goes_cold_without_being_written() {
    let root = TestTempRoot::create("cold-history-open-link-boot");
    let path = root.join("termal.sqlite");
    let workdir = root.to_string_lossy().into_owned();
    let mut inner = StateInner::new();
    let kinds = seed_kinds(&mut inner, &workdir);
    persist_state(&path, &inner).expect("the fixture persists");
    let (loaded, _connection) =
        load_state_from_sqlite_with_timings(&path, true, &mut BootStateLoadTimings::new(false))
            .expect("the fixture boots");
    let mut loaded = loaded.unwrap();
    // An idle child of a live delegation keeps its tail at boot and is
    // queued for the recheck, since its delegation can end in a write that
    // touches only the delegation.
    assert_resident_tail(record(&loaded, &kinds.live_idle));
    assert!(loaded.cold_transcripts.is_candidate(&kinds.live_idle));
    assert!(!loaded.cold_transcripts.is_candidate(&kinds.root));
    let watermark = loaded.last_mutation_stamp;
    loaded.delegations = inner.delegations.clone();
    loaded.delegation_inventory = DelegationInventoryState::Ready;
    drop(loaded.trim_cold_transcript_candidates(watermark));
    assert_resident_tail(record(&loaded, &kinds.live_idle));
    let live = loaded
        .delegations
        .iter()
        .position(|delegation| delegation.id == "d-live")
        .unwrap();
    loaded.delegations[live].status = DelegationStatus::Completed;
    drop(loaded.trim_cold_transcript_candidates(watermark));
    assert_cold(record(&loaded, &kinds.live_idle));
    // A parent of an open delegation never goes cold, and leaves the
    // recheck; one whose delegations are all terminal booted cold.
    assert_cold(record(&loaded, &kinds.done_parent));
    assert_resident_tail(record(&loaded, &kinds.nested_parent));
    assert!(!loaded.cold_transcripts.is_candidate(&kinds.nested_parent));
}

#[test]
fn dropping_the_card_that_held_a_finished_child_wakes_its_recheck() {
    let (mut state, kinds) = seeded_state();
    let (persist_tx, persist_rx) = std::sync::mpsc::channel::<PersistRequest>();
    state.persist_tx = persist_tx;
    persist_and_trim(&state);
    let card = |session_id: &str| TestRunCardRef {
        session_id: session_id.to_owned(),
        message_id: format!("{session_id}-m129"),
        terminal: true,
        failure_detail_version: None,
    };
    let sync = |state: &AppState| {
        let mut inner = state.inner.lock().unwrap();
        state.sync_test_run_cards_traced_locked(
            &mut inner,
            &[],
            &HashSet::new(),
            &HashMap::new(),
            &|_, _| {},
        );
        inner.test_run_cards.is_empty()
    };
    // A terminal card whose run left the index goes from it in a scan that
    // writes no session.
    state
        .inner
        .lock()
        .unwrap()
        .test_run_cards
        .insert("run-root".to_owned(), card(&kinds.root));
    while persist_rx.try_recv().is_ok() {}
    assert!(sync(&state));
    assert!(
        persist_rx.try_recv().is_err(),
        "a card of a session no recheck waits for wakes nothing"
    );
    {
        let mut inner = state.inner.lock().unwrap();
        inner
            .test_run_cards
            .insert("run-child".to_owned(), card(&kinds.completed));
        inner
            .cold_transcripts
            .enroll_candidates([kinds.completed.clone()]);
    }
    assert!(sync(&state));
    assert!(matches!(persist_rx.try_recv(), Ok(PersistRequest::Delta)));
}

#[test]
fn tail_and_history_reads_after_a_real_boot_return_the_stored_transcript_for_every_session_kind() {
    let (state, kinds) = seeded_state();
    let originals = seeded_bodies(&state, &kinds);
    persist_and_trim(&state);
    state.shutdown_persist_blocking();
    let restarted = AppState::new_with_paths(
        state.default_workdir.clone(),
        state.persistence_path.as_ref().clone(),
        state.orchestrator_templates_path.as_ref().clone(),
    )
    .expect("the store boots");
    // Whatever boot recovery appended is durable before anything is compared.
    restarted.shutdown_persist_blocking();
    for id in kinds.all() {
        let count = {
            let inner = restarted.inner.lock().unwrap();
            session_message_count(record(&inner, id)) as usize
        };
        // The stored transcript keeps every original body; boot recovery may
        // only have appended after it.
        let stored = bodies(
            load_persisted_message_range(restarted.persistence_path.as_path(), id, 0, count)
                .unwrap()
                .into_iter()
                .map(|(_, message)| message),
        );
        assert_eq!(stored.len(), count, "stored transcript of `{id}`");
        assert_eq!(
            stored[..TRANSCRIPT_LEN],
            originals[id][..],
            "originals of `{id}`"
        );
        for limit in [1, 20, SQLITE_SESSION_TAIL_MESSAGES] {
            let tail = restarted.get_session_tail(id, limit).unwrap();
            assert_eq!(tail.session.message_count as usize, count);
            assert_eq!(
                bodies(tail.session.messages.iter().cloned()),
                stored[count - limit..].to_vec(),
                "tail {limit} of `{id}` after boot"
            );
        }
        let before = format!("{id}-m050");
        for (query, range) in [
            (selector(None, Some(0), None, 40), 0..40),
            (selector(None, Some(50), None, 40), 50..90),
            (selector(Some(&before), None, None, 10), 40..50),
            (selector(None, None, Some(100), 20), 90..110),
        ] {
            let page = restarted
                .capture_local_session_history(id, query)
                .unwrap()
                .page(id, query, restarted.server_instance_id.clone())
                .unwrap();
            assert_eq!(page.message_start_index, range.start, "history of `{id}`");
            assert_eq!(
                bodies(page.messages.iter().cloned()),
                stored[range].to_vec(),
                "history bodies of `{id}` after boot"
            );
        }
    }
    // The finished children booted cold and reading them kept them cold.
    {
        let inner = restarted.inner.lock().unwrap();
        for id in kinds.finished() {
            assert_cold(record(&inner, id));
        }
    }
}

fn open_write() -> AcceptanceEvaluationSubmission {
    AcceptanceEvaluationSubmission::Pending {
        payload_digest: "digest".to_owned(),
        started_at: stamp_now(),
        original: AcceptanceEvaluationOpenWrite::default(),
    }
}

fn recorded_write() -> AcceptanceEvaluationSubmission {
    AcceptanceEvaluationSubmission::Recorded {
        receipt: AcceptanceEvaluationReceiptExtract::default(),
        recorded_at: stamp_now(),
    }
}

/// Makes `delegation_id` an evaluator that ended while its write was open, as
/// the host settles one: failed, with the host's unavailable result. Its write
/// is then `submission`.
fn fail_as_evaluator_with_unconfirmed_result(
    inner: &mut StateInner,
    delegation_id: &str,
    submission: AcceptanceEvaluationSubmission,
) -> usize {
    let index = inner.find_delegation_index(delegation_id).unwrap();
    let delegation = &mut inner.delegations[index];
    delegation.mode = DelegationMode::Evaluator;
    delegation.status = DelegationStatus::Failed;
    delegation.acceptance_evaluation = Some(DelegationAcceptanceEvaluation {
        submission: open_write(),
        ..evaluation_target(delegation_id, 1)
    });
    let (summary, finding) = acceptance_evaluator_unavailable_result(delegation).unwrap();
    delegation.result = Some(
        serde_json::from_value(serde_json::json!({
            "delegationId": delegation.id,
            "childSessionId": delegation.child_session_id,
            "status": "failed",
            "summary": summary,
            "findings": [{ "severity": "Unavailable", "message": finding }],
        }))
        .unwrap(),
    );
    delegation
        .acceptance_evaluation
        .as_mut()
        .unwrap()
        .submission = submission;
    index
}

#[test]
fn a_finished_parent_stays_resident_until_its_evaluators_card_repair_lands() {
    let (state, kinds) = seeded_state();
    let index = fail_as_evaluator_with_unconfirmed_result(
        &mut state.inner.lock().unwrap(),
        "d-done",
        recorded_write(),
    );
    let watermark = persist_and_trim(&state);
    {
        let mut inner = state.inner.lock().unwrap();
        // The write is recorded, and the refresh that replaces the unavailable
        // result rewrites the parent's card of it; the evaluator itself is a
        // finished child.
        assert_resident_tail(record(&inner, &kinds.done_parent));
        assert!(inner.cold_transcripts.is_candidate(&kinds.done_parent));
        assert_cold(record(&inner, &kinds.done_child));
        drop(inner.trim_cold_transcript_candidates(watermark));
        assert_resident_tail(record(&inner, &kinds.done_parent));
        // The refresh completes the evaluator and queues the parent's
        // recheck itself, though it writes only the delegation.
        inner.cold_transcripts = ColdTranscriptState::default();
        assert!(refresh_recorded_acceptance_result_locked(&mut inner, index).is_some());
        assert_eq!(inner.delegations[index].status, DelegationStatus::Completed);
        assert!(inner.cold_transcripts.is_candidate(&kinds.done_parent));
    }
    // The pass that persists the delegation trims the parent.
    persistence_pass(&state, watermark);
    assert_cold(record(&state.inner.lock().unwrap(), &kinds.done_parent));
}

#[test]
fn a_settled_evaluator_submission_wakes_the_recheck_of_its_finished_parent() {
    let (mut state, kinds) = seeded_state();
    let (persist_tx, persist_rx) = std::sync::mpsc::channel::<PersistRequest>();
    state.persist_tx = persist_tx;
    // The evaluator ended while its submission is still in progress.
    let submission = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index("d-done").unwrap();
        let delegation = &mut inner.delegations[index];
        delegation.mode = DelegationMode::Evaluator;
        delegation.acceptance_evaluation = Some(evaluation_target("d-done", 1));
        AcceptanceEvaluationSubmissionInFlight::admit_locked(&state, &mut inner, "d-done").unwrap()
    };
    let watermark = persist_and_trim(&state);
    {
        let mut inner = state.inner.lock().unwrap();
        assert_resident_tail(record(&inner, &kinds.done_parent));
        inner.cold_transcripts = ColdTranscriptState::default();
    }
    while persist_rx.try_recv().is_ok() {}
    // The submission settles. Leaving the in-progress set writes nothing, and
    // the parent has no card for the refresh to rewrite; its window is held
    // until that refresh is done, and releasing it wakes the recheck.
    drop(submission);
    assert!(matches!(persist_rx.try_recv(), Ok(PersistRequest::Delta)));
    {
        let inner = state.inner.lock().unwrap();
        assert!(!inner.cold_transcripts.has_pins());
        assert!(inner.cold_transcripts.is_candidate(&kinds.done_parent));
    }
    // The card lookup stamped the parent, so the pass that wake runs writes
    // it, and trims it.
    persistence_pass(&state, watermark);
    assert_cold(record(&state.inner.lock().unwrap(), &kinds.done_parent));
}

#[test]
fn a_recorded_write_awaiting_its_projection_keeps_its_finished_parent_resident_at_boot() {
    let root = TestTempRoot::create("cold-history-evaluator-boot");
    let path = root.join("termal.sqlite");
    let workdir = root.to_string_lossy().into_owned();
    let mut inner = StateInner::new();
    let kinds = seed_kinds(&mut inner, &workdir);
    let index = fail_as_evaluator_with_unconfirmed_result(&mut inner, "d-done", recorded_write());
    persist_state(&path, &inner).expect("the fixture persists");
    BOOT_TAIL_LOADS.with(|loads| loads.borrow_mut().clear());
    let (loaded, _connection) =
        load_state_from_sqlite_with_timings(&path, true, &mut BootStateLoadTimings::new(false))
            .expect("the fixture boots");
    let mut loaded = loaded.unwrap();
    let tail_loads = BOOT_TAIL_LOADS.with(|loads| loads.borrow_mut().split_off(0));
    assert!(tail_loads.contains(&kinds.done_parent));
    assert!(!tail_loads.contains(&kinds.done_child));
    assert_resident_tail(record(&loaded, &kinds.done_parent));
    assert_cold(record(&loaded, &kinds.done_child));
    // Boot queues it for the recheck, which keeps it until the evaluator's
    // card is settled and then trims it.
    assert!(loaded.cold_transcripts.is_candidate(&kinds.done_parent));
    let watermark = loaded.last_mutation_stamp;
    loaded.delegations = inner.delegations.clone();
    loaded.delegation_inventory = DelegationInventoryState::Ready;
    drop(loaded.trim_cold_transcript_candidates(watermark));
    assert_resident_tail(record(&loaded, &kinds.done_parent));
    loaded.delegations[index].status = DelegationStatus::Completed;
    drop(loaded.trim_cold_transcript_candidates(watermark));
    assert_cold(record(&loaded, &kinds.done_parent));
}

#[test]
fn an_ended_evaluators_open_write_leaves_its_finished_parent_cold_at_boot() {
    let root = TestTempRoot::create("cold-history-open-write-boot");
    let path = root.join("termal.sqlite");
    let workdir = root.to_string_lossy().into_owned();
    let mut inner = StateInner::new();
    let kinds = seed_kinds(&mut inner, &workdir);
    // Only a replay settles this write, and only a root parent may request
    // one: nothing can rewrite the nested parent's card of it.
    fail_as_evaluator_with_unconfirmed_result(&mut inner, "d-done", open_write());
    persist_state(&path, &inner).expect("the fixture persists");
    BOOT_TAIL_LOADS.with(|loads| loads.borrow_mut().clear());
    let (loaded, _connection) =
        load_state_from_sqlite_with_timings(&path, true, &mut BootStateLoadTimings::new(false))
            .expect("the fixture boots");
    let loaded = loaded.unwrap();
    let tail_loads = BOOT_TAIL_LOADS.with(|loads| loads.borrow_mut().split_off(0));
    assert!(!tail_loads.contains(&kinds.done_parent));
    assert_cold(record(&loaded, &kinds.done_parent));
}

fn delegation_card_status(
    state: &AppState,
    parent: &str,
    delegation_id: &str,
) -> Option<ParallelAgentStatus> {
    let inner = state.inner.lock().unwrap();
    record(&inner, parent)
        .session
        .messages
        .iter()
        .find_map(|message| match message {
            Message::ParallelAgents { agents, .. } => agents
                .iter()
                .find(|agent| agent.id == delegation_id)
                .map(|agent| agent.status),
            _ => None,
        })
}

#[test]
fn a_follow_up_of_a_cold_parents_delegation_rewrites_its_card() {
    let state = test_app_state_with_drained_delegation_codex_runtime("cold-history-nested");
    let root = test_session_id(&state, Agent::Codex);
    let explorer = |parent: &str, title: &str| {
        let created = state
            .create_read_only_delegation(
                parent,
                CreateDelegationRequest {
                    prompt: format!("{title} task."),
                    title: Some(title.to_owned()),
                    cwd: None,
                    agent: Some(Agent::Codex),
                    model: None,
                    mode: Some(DelegationMode::Reviewer),
                    write_policy: Some(DelegationWritePolicy::ReadOnly),
                },
            )
            .expect("delegation should be created");
        mark_delegation_as_unstructured_explorer(&state, &created.delegation.id);
        created.delegation
    };
    // A child that delegated in turn, both finished.
    let outer = explorer(&root, "Outer");
    let nested = explorer(&outer.child_session_id, "Nested");
    for child in [&nested.child_session_id, &outer.child_session_id] {
        finish_delegation_child_with_assistant_text(&state, child, "Done.");
        state
            .refresh_delegation_for_child_session(child)
            .expect("refresh should complete");
    }
    let parent = outer.child_session_id.as_str();
    assert_eq!(
        delegation_card_status(&state, parent, &nested.id),
        Some(ParallelAgentStatus::Completed)
    );
    // The parent keeps no resident transcript, as a persistence pass leaves it.
    {
        let mut inner = state.inner.lock().unwrap();
        persist_state(state.persistence_path.as_ref(), &inner).unwrap();
        let index = inner.find_session_index(parent).unwrap();
        drop(detach_retained_session_messages(
            &mut inner.sessions[index],
            0,
        ));
    }
    assert_eq!(delegation_card_status(&state, parent, &nested.id), None);
    // A message to the nested child re-arms its delegation, and the card in
    // the parent's transcript says so.
    state
        .followup_delegation_request(
            parent,
            &nested.id,
            SendMessageRequest {
                text: "Continue.".to_owned(),
                expanded_text: None,
                source_session_id: None,
                source_mailbox: None,
                attachments: vec![],
            },
        )
        .expect("the follow-up is admitted");
    assert_eq!(
        delegation_card_status(&state, parent, &nested.id),
        Some(ParallelAgentStatus::Running)
    );
}

/// Every page of a delegation's result output, or the refusal, without the
/// fields that name the serving process.
fn result_output(state: &AppState, parent: &str, delegation_id: &str) -> Vec<serde_json::Value> {
    let mut pages = Vec::new();
    let mut offset = 0;
    loop {
        match state.get_delegation_result_output(
            parent,
            delegation_id,
            offset,
            DEFAULT_DELEGATION_RESULT_OUTPUT_PAGE_BYTES,
        ) {
            Ok(page) => {
                let next = page.next_offset_bytes;
                let mut value = serde_json::to_value(&page).unwrap();
                let fields = value.as_object_mut().unwrap();
                fields.remove("revision");
                fields.remove("serverInstanceId");
                pages.push(value);
                match next {
                    Some(next) => offset = next,
                    None => return pages,
                }
            }
            Err(error) => {
                pages.push(serde_json::json!({
                    "status": error.status.as_u16(),
                    "message": error.message,
                }));
                return pages;
            }
        }
    }
}

#[test]
fn result_output_reads_after_a_real_boot_equal_the_resident_reads_for_every_session_kind() {
    let (state, kinds) = seeded_state();
    {
        // Each finished child returns its transcript's output.
        let mut inner = state.inner.lock().unwrap();
        for delegation in inner.delegations.iter_mut() {
            if delegation_is_terminal(delegation.status) {
                delegation.mode = DelegationMode::Explorer;
                delegation.result = Some(
                    serde_json::from_value(serde_json::json!({
                        "delegationId": delegation.id,
                        "childSessionId": delegation.child_session_id,
                        "status": delegation.status,
                        "summary": format!("{} finished", delegation.id),
                    }))
                    .unwrap(),
                );
            }
        }
    }
    persist_and_trim(&state);
    state.shutdown_persist_blocking();
    let restarted = AppState::new_with_paths(
        state.default_workdir.clone(),
        state.persistence_path.as_ref().clone(),
        state.orchestrator_templates_path.as_ref().clone(),
    )
    .expect("the store boots");
    restarted.shutdown_persist_blocking();
    // The output of every child's delegation, read through the root or a
    // nested parent, and the dangling child's missing one.
    let mut links = restarted
        .inner
        .lock()
        .unwrap()
        .delegations
        .iter()
        .map(|delegation| {
            (
                delegation.parent_session_id.clone(),
                delegation.id.clone(),
                delegation.child_session_id.clone(),
            )
        })
        .collect::<Vec<_>>();
    links.push((
        kinds.root.clone(),
        "d-missing".to_owned(),
        kinds.dangling.clone(),
    ));
    let children = links
        .iter()
        .map(|(_, _, child)| child)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        children,
        kinds
            .all()
            .into_iter()
            .filter(|id| **id != kinds.root)
            .collect::<BTreeSet<_>>()
    );
    let reads = |state: &AppState| {
        links
            .iter()
            .map(|(parent, id, _)| (id.clone(), result_output(state, parent, id)))
            .collect::<Vec<_>>()
    };
    let cold = reads(&restarted);
    {
        let inner = restarted.inner.lock().unwrap();
        for id in kinds.finished() {
            assert_cold(record(&inner, id));
        }
    }
    // The same reads with every last window resident, as the finished
    // children kept it before they went cold.
    let pins = kinds
        .all()
        .into_iter()
        .map(|id| restarted.hydrate_cold_session_window(id).unwrap())
        .collect::<Vec<_>>();
    {
        let inner = restarted.inner.lock().unwrap();
        for id in kinds.finished() {
            assert_resident_tail(record(&inner, id));
        }
    }
    let resident = reads(&restarted);
    drop(pins);
    assert_eq!(cold, resident);
    for (id, pages) in &cold {
        if ["d-completed", "d-failed", "d-done-own", "d-done"].contains(&id.as_str()) {
            assert!(
                pages[0].get("output").is_some(),
                "a finished child's output is served: {pages:?}"
            );
        }
    }
}
