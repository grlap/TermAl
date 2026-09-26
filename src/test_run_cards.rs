// The test-run card (docs/features/test-runs.md, slice 2 "The test-run card"):
// a persisted `testRun` message in the owner session's transcript that the host
// creates when the index first sees a run and updates in place from the index.
//
// Owns: the card wire types (TestRunCardSnapshot and friends), the persisted
// card map entry and the epoch, the snapshot builder with its byte budgets,
// the failure excerpt read from results.json, the rule for when a card is
// created, and applying card creation and updates to transcripts under the
// state lock. Also the durable epoch at boot.
//
// Does not own: reading run directories (test_runs_disk.rs), the index and its
// rescan (test_runs.rs, which calls into this file), run waits, or rendering
// (ui/, Termal::Codex). A card whose message has left the session's in-memory
// transcript window is not updated by this changeset (see the doc's Card
// updates section).
//
// New module, not split from another file.

/// The card message's `schemaVersion`.
const TEST_RUN_CARD_SCHEMA_VERSION: u32 = 1;
/// The serialized core of a snapshot is never cut; a run whose core exceeds
/// this gets no card.
const TEST_RUN_CARD_CORE_MAX_BYTES: usize = 10 * 1024;
/// The optional part is filled in a fixed order within this budget.
const TEST_RUN_CARD_OPTIONAL_MAX_BYTES: usize = 8 * 1024;
/// A failure excerpt is cut to this many bytes at a character boundary.
const TEST_RUN_CARD_EXCERPT_MAX_BYTES: usize = 4 * 1024;
/// A run already terminal at first sight still gets a card when it started at
/// most this long before: a short focused run that ended between two rescans.
const TEST_RUN_CARD_LATE_SIGHT_WINDOW: chrono::TimeDelta = chrono::TimeDelta::minutes(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum TestRunCardUnknownReason {
    ProcessGone,
    ResultsUnreadable,
    NoPid,
    /// The run left the index before a terminal result.
    NotIndexed,
}

impl From<TestRunUnknownReason> for TestRunCardUnknownReason {
    fn from(reason: TestRunUnknownReason) -> Self {
        match reason {
            TestRunUnknownReason::ProcessGone => Self::ProcessGone,
            TestRunUnknownReason::ResultsUnreadable => Self::ResultsUnreadable,
            TestRunUnknownReason::NoPid => Self::NoPid,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum TestRunCardFailurePhase {
    Stage,
    Preflight,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunCardFailure {
    phase: TestRunCardFailurePhase,
    /// The first failing stage or preflight check.
    name: String,
    /// Its diagnostics text, at most 4 KiB.
    excerpt: String,
    truncated: bool,
}

/// What a card shows, read only from here by the renderer. It has no volatile
/// field (no detail version, no elapsed time), so an unchanged run never
/// rewrites it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunCardSnapshot {
    // Core: never cut.
    run_id: String,
    worktree: String,
    run_dir: String,
    preset: TestRunPreset,
    detached: Option<bool>,
    state: TestRunState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unknown_reason: Option<TestRunCardUnknownReason>,
    interrupted: bool,
    current_stage: Option<String>,
    started_at: Option<String>,
    ended_at: Option<String>,
    exit_code: Option<i64>,
    // Optional part: filled in priority order within its budget.
    stages: Vec<TestRunStageSummary>,
    stages_omitted: usize,
    error: Option<String>,
    error_truncated: bool,
    failure: Option<TestRunCardFailure>,
    command: Option<Vec<String>>,
    command_truncated: bool,
}

impl TestRunCardSnapshot {
    fn is_terminal(&self) -> bool {
        matches!(self.state, TestRunState::Passed | TestRunState::Failed)
    }
}

/// Where a run's card lives, persisted in the `testRunCards` map by run id.
/// The transcript stays the render source; boot never scans transcripts.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunCardRef {
    session_id: String,
    message_id: String,
    /// Whether the stored snapshot is terminal. A card that is not is
    /// checked after every scan and keeps its run in the index.
    #[serde(default)]
    terminal: bool,
    /// The `detailVersion` the stored failure excerpt was read from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure_detail_version: Option<String>,
}

/// When this host process first saw a run.
#[derive(Clone, Copy, Debug)]
struct TestRunFirstSight {
    at: chrono::DateTime<chrono::Utc>,
    non_terminal: bool,
}

fn test_run_card_json_len<T: Serialize>(value: &T) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

fn test_run_card_cut(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_owned(), false);
    }
    (test_run_truncate(text, max), true)
}

/// A run's card snapshot, or `None` when its core alone exceeds 10 KiB (only
/// possible with heavily escaped paths). The optional part is filled in the
/// contract's fixed order, so the same run always gives the same snapshot:
/// the failed and the running stage, `error`, `failure` (its excerpt cut to
/// fit), `command` whole or not at all, then the other stages in plan order.
fn test_run_card_snapshot(
    entry: &TestRunEntry,
    failure: Option<TestRunCardFailure>,
) -> Option<TestRunCardSnapshot> {
    let summary = &entry.summary;
    let mut snapshot = TestRunCardSnapshot {
        run_id: summary.run_id.clone(),
        worktree: summary.worktree.clone(),
        run_dir: summary.run_dir.clone(),
        preset: summary.preset,
        detached: summary.detached,
        state: summary.state,
        unknown_reason: summary
            .unknown_reason
            .filter(|_| summary.state == TestRunState::Unknown)
            .map(TestRunCardUnknownReason::from),
        interrupted: summary.interrupted,
        current_stage: summary.current_stage.clone(),
        started_at: summary.started_at.clone(),
        ended_at: summary.ended_at.clone(),
        exit_code: summary.exit_code,
        stages: Vec::new(),
        stages_omitted: 0,
        error: None,
        error_truncated: false,
        failure: None,
        command: None,
        command_truncated: false,
    };
    let core = test_run_card_json_len(&snapshot);
    if core > TEST_RUN_CARD_CORE_MAX_BYTES {
        return None;
    }
    let fits = |snapshot: &TestRunCardSnapshot| {
        test_run_card_json_len(snapshot).saturating_sub(core) <= TEST_RUN_CARD_OPTIONAL_MAX_BYTES
    };
    let plan = &summary.stages;
    let mut kept = vec![false; plan.len()];
    let place = |snapshot: &mut TestRunCardSnapshot, kept: &[bool]| {
        snapshot.stages = plan
            .iter()
            .zip(kept)
            .filter(|(_, keep)| **keep)
            .map(|(stage, _)| stage.clone())
            .collect();
    };

    // 1. The failed stage and the running stage.
    for (index, stage) in plan.iter().enumerate() {
        if matches!(stage.state, TestRunStageState::Failed | TestRunStageState::Running) {
            kept[index] = true;
            place(&mut snapshot, &kept);
            if !fits(&snapshot) {
                kept[index] = false;
                place(&mut snapshot, &kept);
            }
        }
    }
    // 2. The error, measured against the original results.json error.
    let original_error_truncated = entry
        .disk
        .results
        .as_ref()
        .is_some_and(|results| results.error_truncated);
    if let Some(error) = summary.error.clone() {
        snapshot.error = Some(error);
        snapshot.error_truncated = original_error_truncated;
        if !fits(&snapshot) {
            snapshot.error = None;
            snapshot.error_truncated = true;
        }
    }
    // 3. The failure, terminal failed runs only; the excerpt is cut to fit.
    if let Some(mut failure) = failure.filter(|_| summary.state == TestRunState::Failed) {
        let full_excerpt = failure.excerpt.clone();
        snapshot.failure = Some(failure.clone());
        if !fits(&snapshot) {
            // Find the longest prefix that fits, by halving.
            let (mut low, mut high) = (0, full_excerpt.len());
            while low < high {
                let mid = (low + high).div_ceil(2);
                failure.excerpt = test_run_truncate(&full_excerpt, mid);
                snapshot.failure = Some(failure.clone());
                if fits(&snapshot) {
                    low = mid;
                } else {
                    high = mid - 1;
                }
            }
            failure.excerpt = test_run_truncate(&full_excerpt, low);
            failure.truncated = true;
            snapshot.failure = Some(failure);
            if !fits(&snapshot) {
                snapshot.failure = None;
            }
        }
    }
    // 4. The command, whole or not at all.
    snapshot.command_truncated = summary.command_truncated;
    if let Some(command) = summary.command.clone() {
        snapshot.command = Some(command);
        if !fits(&snapshot) {
            snapshot.command = None;
            snapshot.command_truncated = true;
        }
    }
    // 5. The other stages in plan order; the first that does not fit ends
    // the list, and every stage left out is counted.
    for index in 0..plan.len() {
        if kept[index] {
            continue;
        }
        kept[index] = true;
        place(&mut snapshot, &kept);
        if !fits(&snapshot) {
            kept[index] = false;
            place(&mut snapshot, &kept);
            break;
        }
    }
    snapshot.stages_omitted = kept.iter().filter(|keep| !**keep).count();
    Some(snapshot)
}

/// One read of a failed run's first failure.
#[derive(Clone, Debug)]
enum TestRunCardFailureRead {
    /// results.json was read: the digest of the bytes read (the version the
    /// excerpt belongs to) and the failure found in them, if any.
    Read {
        digest: String,
        failure: Option<TestRunCardFailure>,
    },
    /// results.json could not be read whole or parsed this time (a
    /// replacement race, or a file over the read limit). Never recorded as
    /// an answer: the card keeps its previous excerpt and the read is retried.
    Unavailable,
}

/// The first failure of a terminal failed run, read from its results.json
/// with the detail route's 1 MiB guard: the first failing stage, else the
/// first failing preflight check. Its diagnostics text is cut to 4 KiB.
fn test_run_card_failure(run_dir: &FsPath) -> TestRunCardFailureRead {
    test_run_failure_read(run_dir, TEST_RUN_CARD_EXCERPT_MAX_BYTES)
}

/// `test_run_card_failure` with the diagnostics cut to `max_excerpt` bytes:
/// a run wait's resume prompt allows more than a card (test_run_waits.rs).
fn test_run_failure_read(run_dir: &FsPath, max_excerpt: usize) -> TestRunCardFailureRead {
    let TestRunJson::Parsed(results, digest, _) =
        test_run_read_json(&run_dir.join("results.json"))
    else {
        return TestRunCardFailureRead::Unavailable;
    };
    let failed = |key: &str, phase: TestRunCardFailurePhase| {
        results
            .get(key)
            .and_then(Value::as_array)?
            .iter()
            .find(|item| match phase {
                TestRunCardFailurePhase::Stage => {
                    item.get("state").and_then(Value::as_str) == Some("failed")
                }
                TestRunCardFailurePhase::Preflight => item
                    .get("code")
                    .and_then(Value::as_i64)
                    .is_some_and(|code| code != 0),
            })
            .and_then(|item| {
                let name = test_run_str(item, "name").filter(|name| test_run_valid_stage_name(name))?;
                let diagnostics = test_run_diagnostics(item.get("diagnostics"));
                let (excerpt, cut) = test_run_card_cut(
                    diagnostics.as_ref().map_or("", |diagnostics| &diagnostics.text),
                    max_excerpt,
                );
                Some(TestRunCardFailure {
                    phase,
                    name,
                    excerpt,
                    truncated: cut || diagnostics.is_some_and(|diagnostics| diagnostics.truncated),
                })
            })
    };
    let failure = failed("stages", TestRunCardFailurePhase::Stage)
        .or_else(|| failed("preflight", TestRunCardFailurePhase::Preflight));
    TestRunCardFailureRead::Read { digest, failure }
}

/// A one-line preview for the session list and sidebar.
fn test_run_card_preview_text(run: &TestRunCardSnapshot) -> String {
    let preset = match run.preset {
        TestRunPreset::Full => "full",
        TestRunPreset::Focused => "focused",
        TestRunPreset::Live => "live",
    };
    let state = match run.state {
        TestRunState::Running => "running",
        TestRunState::Passed => "passed",
        TestRunState::Failed => "failed",
        TestRunState::Unknown => "unknown",
    };
    make_preview(&format!("Test run ({preset}): {state}"))
}

/// An RFC 3339 time as the launcher writes it, or `None`.
fn test_run_card_time(text: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| time.with_timezone(&chrono::Utc))
}

/// Whether a run seen by this index gets a card now: its start is known and
/// at or after the epoch, and it was not terminal at first sight or started
/// at most 10 minutes before it. The owner is checked by the caller.
fn test_run_card_eligible(
    started_at: Option<&str>,
    epoch: &str,
    first_sight: TestRunFirstSight,
) -> bool {
    let (Some(started), Some(epoch)) = (started_at.and_then(test_run_card_time), test_run_card_time(epoch))
    else {
        return false;
    };
    started >= epoch
        && (first_sight.non_terminal
            || started >= first_sight.at - TEST_RUN_CARD_LATE_SIGHT_WINDOW)
}

/// A card delta to publish once its revision is allocated.
enum TestRunCardDelta {
    Created {
        session_id: String,
        message_id: String,
        message_index: usize,
        message_count: u32,
        message: Message,
        preview: String,
        status: SessionStatus,
        session_mutation_stamp: u64,
    },
    Updated {
        session_id: String,
        message_id: String,
        message_index: usize,
        message_count: u32,
        preview: String,
        session_mutation_stamp: u64,
        run: TestRunCardSnapshot,
    },
}

impl TestRunCardDelta {
    fn into_event(self, revision: u64) -> DeltaEvent {
        match self {
            Self::Created {
                session_id,
                message_id,
                message_index,
                message_count,
                message,
                preview,
                status,
                session_mutation_stamp,
            } => DeltaEvent::MessageCreated {
                revision,
                session_id,
                message_id,
                message_index,
                message_count,
                message,
                preview,
                status,
                session_queue: None,
                session_mutation_stamp: Some(session_mutation_stamp),
            },
            Self::Updated {
                session_id,
                message_id,
                message_index,
                message_count,
                preview,
                session_mutation_stamp,
                run,
            } => DeltaEvent::TestRunCardUpdated {
                revision,
                session_id,
                message_id,
                message_index,
                message_count,
                preview,
                session_mutation_stamp: Some(session_mutation_stamp),
                run,
            },
        }
    }
}

/// Appends a new card to its owner's transcript and records it in the map.
/// `None` when the owner cannot hold one: gone, internal, or a remote proxy,
/// whose transcript the remote owns.
fn create_test_run_card_locked(
    inner: &mut StateInner,
    owner_session_id: &str,
    snapshot: TestRunCardSnapshot,
    failure_detail_version: Option<String>,
) -> Option<TestRunCardDelta> {
    let index = inner.find_visible_session_index(owner_session_id)?;
    if inner.sessions[index].remote_id.is_some() {
        return None;
    }
    let message_id = inner.next_message_id();
    let run_id = snapshot.run_id.clone();
    let terminal = snapshot.is_terminal();
    let message = Message::TestRun {
        id: message_id.clone(),
        timestamp: stamp_now(),
        author: Author::System,
        schema_version: TEST_RUN_CARD_SCHEMA_VERSION,
        run: snapshot,
    };
    let record = inner
        .session_mut_by_index(index)
        .expect("session index should be valid");
    if let Some(preview) = message.preview_text() {
        record.session.preview = preview;
    }
    let local_index = push_message_on_record(record, message.clone());
    let delta = TestRunCardDelta::Created {
        session_id: record.session.id.clone(),
        message_id: message_id.clone(),
        message_index: global_message_index(record, local_index),
        message_count: session_message_count(record),
        message,
        preview: record.session.preview.clone(),
        status: record.session.status,
        session_mutation_stamp: record.mutation_stamp,
    };
    inner.test_run_cards.insert(
        run_id,
        TestRunCardRef {
            session_id: owner_session_id.to_owned(),
            message_id,
            terminal,
            failure_detail_version,
        },
    );
    Some(delta)
}

/// What an existing card's lookup found.
enum TestRunCardLookup {
    /// The card's message, in the session's in-memory window.
    Resident(TestRunCardSnapshot),
    /// The session exists, but the message is outside its in-memory window.
    Cold,
    /// The session or the message is gone.
    Gone,
}

fn test_run_card_lookup(inner: &mut StateInner, card: &TestRunCardRef) -> TestRunCardLookup {
    let Some(index) = inner.find_session_index(&card.session_id) else {
        return TestRunCardLookup::Gone;
    };
    let record = &mut inner.sessions[index];
    match message_index_on_record(record, &card.message_id) {
        Some(local_index) => match &record.session.messages[local_index] {
            Message::TestRun { run, .. } => TestRunCardLookup::Resident(run.clone()),
            _ => TestRunCardLookup::Gone,
        },
        None if record.message_start_index > 0 => TestRunCardLookup::Cold,
        None => TestRunCardLookup::Gone,
    }
}

/// Replaces a resident card's snapshot when it differs, and updates the map.
/// `None` when nothing changed.
fn update_test_run_card_locked(
    inner: &mut StateInner,
    run_id: &str,
    snapshot: TestRunCardSnapshot,
    failure_detail_version: Option<String>,
) -> Option<TestRunCardDelta> {
    let card = inner.test_run_cards.get(run_id)?.clone();
    let index = inner.find_session_index(&card.session_id)?;
    // Compared before the record is marked mutated, so an unchanged card is
    // neither persisted nor published.
    let local_index = message_index_on_record(&mut inner.sessions[index], &card.message_id)?;
    let unchanged = match &inner.sessions[index].session.messages[local_index] {
        Message::TestRun { run, .. } => *run == snapshot,
        _ => return None,
    };
    // Bookkeeping follows the check even when the card is unchanged, so an
    // excerpt read at a new version is not read again on every rescan.
    if let Some(stored) = inner.test_run_cards.get_mut(run_id) {
        stored.terminal = snapshot.is_terminal();
        stored.failure_detail_version = failure_detail_version.clone();
    }
    if unchanged {
        return None;
    }
    let record = inner
        .session_mut_by_index(index)
        .expect("session index should be valid");
    if let Message::TestRun { run, .. } = &mut record.session.messages[local_index] {
        *run = snapshot.clone();
    }
    let preview = test_run_card_preview_text(&snapshot);
    record.session.preview = preview.clone();
    let delta = TestRunCardDelta::Updated {
        session_id: record.session.id.clone(),
        message_id: card.message_id.clone(),
        message_index: global_message_index(record, local_index),
        message_count: session_message_count(record),
        preview,
        session_mutation_stamp: record.mutation_stamp,
        run: snapshot.clone(),
    };
    Some(delta)
}

/// Whether a card's message is in its session's in-memory window, without
/// repairing the position cache.
fn test_run_card_is_resident(inner: &StateInner, card: &TestRunCardRef) -> bool {
    inner.find_session_index(&card.session_id).is_some_and(|index| {
        let record = &inner.sessions[index];
        record
            .message_positions
            .get(&card.message_id)
            .and_then(|local_index| record.session.messages.get(*local_index))
            .is_some_and(|message| message.id() == card.message_id)
    })
}

/// Enables card creation once the epoch is durable. Runs first published
/// while it was not are re-evaluated on the next scan, so a short run that
/// started after the epoch and ended inside that window still gets its card.
fn enable_test_run_cards_locked(inner: &mut StateInner) {
    if !inner.test_runs.cards_enabled {
        inner.test_runs.cards_enabled = true;
        inner.test_runs.cards_reevaluate = true;
    }
}

/// Where making the card epoch durable stands.
enum TestRunCardsEpochProgress {
    /// Durable: cards are enabled.
    Durable,
    /// Queued for the persist worker; the waiter resolves when it is written.
    Pending(PersistFenceWaiter),
    /// Could not be persisted this time; the next attempt persists the same
    /// epoch again.
    Unavailable,
}

/// How long one attempt to make the epoch durable may take before the next
/// attempt starts. Rescans never wait for it.
const TEST_RUN_CARDS_EPOCH_FENCE_DEADLINE: Duration = Duration::from_secs(30);

impl AppState {
    /// Starts making the card epoch durable. The first boot with slice 2
    /// stores the current time; later boots load it (durable already) and
    /// never derive it from "now" again. An epoch set earlier in this process
    /// but not yet proven durable is persisted again, never replaced.
    fn begin_test_run_cards_epoch(&self) -> TestRunCardsEpochProgress {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        // Enabled at load when the epoch came from disk.
        if inner.test_runs.cards_enabled {
            return TestRunCardsEpochProgress::Durable;
        }
        let epoch = match inner.test_run_cards_epoch.clone() {
            Some(epoch) => epoch,
            None => {
                let epoch = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                inner.test_run_cards_epoch = Some(epoch.clone());
                epoch
            }
        };
        match self.persist_internal_locked_with_dispatch(&inner) {
            Ok(PersistDispatch::Synchronous) => {
                enable_test_run_cards_locked(&mut inner);
                TestRunCardsEpochProgress::Durable
            }
            Ok(PersistDispatch::BackgroundQueued) => {
                let (fence, waiter) = PersistFence::new(
                    PersistFenceTarget::TestRunCardsEpoch(epoch),
                    std::time::Instant::now() + TEST_RUN_CARDS_EPOCH_FENCE_DEADLINE,
                );
                if self
                    .persist_tx
                    .send(PersistRequest::Fence(Box::new(fence)))
                    .is_err()
                {
                    return TestRunCardsEpochProgress::Unavailable;
                }
                TestRunCardsEpochProgress::Pending(waiter)
            }
            Err(err) => {
                eprintln!("test runs> failed to persist the card epoch: {err:#}");
                TestRunCardsEpochProgress::Unavailable
            }
        }
    }

    /// One non-blocking step toward a durable epoch, taken on every tick of
    /// the rescan thread before its rescan. Returns whether cards are
    /// enabled. It never waits: until the epoch is durable, rescans keep
    /// publishing runs and create no card, so a slow or failing persist can
    /// never starve the index. A fence that failed or expired starts a new
    /// attempt with the same epoch.
    fn step_test_run_cards_epoch(&self, pending: &mut Option<PersistFenceWaiter>) -> bool {
        if let Some(waiter) = pending.take() {
            match waiter.wait_until(std::time::Instant::now()) {
                None => {
                    *pending = Some(waiter);
                    return false;
                }
                Some(Ok(())) => {
                    enable_test_run_cards_locked(&mut self.inner.lock().expect("state mutex poisoned"));
                    return true;
                }
                Some(Err(err)) => {
                    eprintln!("test runs> the card epoch is not durable yet ({err:?}); retrying");
                }
            }
        }
        match self.begin_test_run_cards_epoch() {
            TestRunCardsEpochProgress::Durable => true,
            TestRunCardsEpochProgress::Pending(waiter) => {
                *pending = Some(waiter);
                false
            }
            TestRunCardsEpochProgress::Unavailable => false,
        }
    }

    /// `step_test_run_cards_epoch` until it settles, for tests that need
    /// cards enabled. Production never blocks on the epoch.
    #[cfg(test)]
    fn ensure_test_run_cards_epoch(&self) -> bool {
        match self.begin_test_run_cards_epoch() {
            TestRunCardsEpochProgress::Durable => true,
            TestRunCardsEpochProgress::Pending(waiter) => {
                let durable = waiter.wait().is_ok();
                if durable {
                    enable_test_run_cards_locked(&mut self.inner.lock().expect("state mutex poisoned"));
                }
                durable
            }
            TestRunCardsEpochProgress::Unavailable => false,
        }
    }

    /// Creates and updates cards for this rescan, under the lock that
    /// committed its run deltas. `changed` names the runs whose summary
    /// changed and was published; `failures` holds excerpts read before the
    /// lock. Every card delta is published under the lock that allocated its
    /// revision.
    fn sync_test_run_cards_locked(
        &self,
        inner: &mut StateInner,
        entries: &[TestRunEntry],
        changed: &HashSet<String>,
        failures: &HashMap<String, TestRunCardFailureRead>,
        publish: &dyn Fn(&DeltaEvent),
    ) {
        let now = chrono::Utc::now();
        let indexed: HashSet<&str> = entries
            .iter()
            .map(|entry| entry.summary.run_id.as_str())
            .collect();
        // Once, on the scan after cards became enabled: runs first published
        // while they were not get a card if they qualify.
        let reevaluate = inner.test_runs.cards_enabled
            && std::mem::take(&mut inner.test_runs.cards_reevaluate);
        let mut deltas = Vec::new();
        for entry in entries {
            let run_id = &entry.summary.run_id;
            let first_sight = *inner
                .test_runs
                .first_sight
                .entry(run_id.clone())
                .or_insert(TestRunFirstSight {
                    at: now,
                    non_terminal: !entry.disk.is_terminal(),
                });
            let carded = inner.test_run_cards.contains_key(run_id);
            // A carded run whose excerpt was read (or was due and could not
            // be) is applied even when its summary did not change, so a
            // pending read is retried until it lands.
            if !changed.contains(run_id)
                && !(carded && failures.contains_key(run_id))
                && !(reevaluate && !carded)
            {
                continue;
            }
            let is_failed = entry.summary.state == TestRunState::Failed;
            if let Some(card) = inner.test_run_cards.get(run_id).cloned() {
                let lookup = test_run_card_lookup(inner, &card);
                let stored = match lookup {
                    TestRunCardLookup::Resident(stored) => stored,
                    // Out of the in-memory window: not updated by this
                    // changeset. The run keeps its entry for a later pass.
                    TestRunCardLookup::Cold => continue,
                    TestRunCardLookup::Gone => {
                        inner.test_run_cards.remove(run_id);
                        continue;
                    }
                };
                let (failure, failure_version) = match failures.get(run_id) {
                    _ if !is_failed => (None, None),
                    Some(TestRunCardFailureRead::Read { digest, failure }) => {
                        (failure.clone(), Some(digest.clone()))
                    }
                    // Not due (read at this version already), or unreadable
                    // this time: keep the stored excerpt and its version.
                    Some(TestRunCardFailureRead::Unavailable) | None => {
                        (stored.failure.clone(), card.failure_detail_version.clone())
                    }
                };
                if let Some(snapshot) = test_run_card_snapshot(entry, failure) {
                    deltas.extend(update_test_run_card_locked(
                        inner,
                        run_id,
                        snapshot,
                        failure_version,
                    ));
                }
                continue;
            }
            if !inner.test_runs.cards_enabled {
                continue;
            }
            let (Some(owner), Some(epoch)) = (
                entry.summary.owner_session_id.clone(),
                inner.test_run_cards_epoch.clone(),
            ) else {
                continue;
            };
            if !test_run_card_eligible(entry.summary.started_at.as_deref(), &epoch, first_sight) {
                continue;
            }
            // The excerpt's version is recorded only when it was read, so an
            // unread one is read and applied on a later rescan.
            let (failure, failure_version) = match failures.get(run_id).filter(|_| is_failed) {
                Some(TestRunCardFailureRead::Read { digest, failure }) => {
                    (failure.clone(), Some(digest.clone()))
                }
                Some(TestRunCardFailureRead::Unavailable) | None => (None, None),
            };
            if let Some(snapshot) = test_run_card_snapshot(entry, failure) {
                deltas.extend(create_test_run_card_locked(
                    inner,
                    &owner,
                    snapshot,
                    failure_version,
                ));
            }
        }
        // A card that is not terminal and whose run is no longer indexed
        // reads unknown (notIndexed), so no card stays running after its
        // directory is gone. If the run is indexed again, its summary updates
        // the card again. The transcript decides whether a card is terminal:
        // the map's flag is only a hint (reset at load), so a card whose
        // stored snapshot is terminal is left alone and its flag corrected.
        let orphaned: Vec<(String, TestRunCardRef)> = inner
            .test_run_cards
            .iter()
            .filter(|(run_id, card)| !card.terminal && !indexed.contains(run_id.as_str()))
            .map(|(run_id, card)| (run_id.clone(), card.clone()))
            .collect();
        for (run_id, card) in orphaned {
            match test_run_card_lookup(inner, &card) {
                TestRunCardLookup::Resident(stored) if stored.is_terminal() => {
                    if let Some(entry) = inner.test_run_cards.get_mut(&run_id) {
                        entry.terminal = true;
                    }
                }
                TestRunCardLookup::Resident(stored) => {
                    let snapshot = TestRunCardSnapshot {
                        state: TestRunState::Unknown,
                        unknown_reason: Some(TestRunCardUnknownReason::NotIndexed),
                        ..stored
                    };
                    deltas.extend(update_test_run_card_locked(
                        inner,
                        &run_id,
                        snapshot,
                        card.failure_detail_version.clone(),
                    ));
                }
                TestRunCardLookup::Cold => {}
                TestRunCardLookup::Gone => {
                    inner.test_run_cards.remove(&run_id);
                }
            }
        }
        // A terminal card whose run left the index needs no more updates, so
        // its entry goes: the map stays bounded by the index, and a deleted
        // owner's entries go with their runs. A run that comes back after that
        // gets no second card, because it is old at its new first sight; so an
        // entry stays while its run started within the late-sight window,
        // where a returning run would still qualify.
        let finished: Vec<String> = inner
            .test_run_cards
            .iter()
            .filter(|(run_id, card)| card.terminal && !indexed.contains(run_id.as_str()))
            .map(|(run_id, _)| run_id.clone())
            .collect();
        for run_id in finished {
            let card = inner.test_run_cards[&run_id].clone();
            let recent = match test_run_card_lookup(inner, &card) {
                TestRunCardLookup::Resident(stored) => stored
                    .started_at
                    .as_deref()
                    .and_then(test_run_card_time)
                    .is_some_and(|started| started >= now - TEST_RUN_CARD_LATE_SIGHT_WINDOW),
                TestRunCardLookup::Cold | TestRunCardLookup::Gone => false,
            };
            if !recent {
                inner.test_run_cards.remove(&run_id);
            }
        }
        // First sights are kept only for runs still indexed.
        inner
            .test_runs
            .first_sight
            .retain(|run_id, _| indexed.contains(run_id.as_str()));
        for delta in deltas {
            match self.commit_persisted_delta_locked(inner) {
                Ok(revision) => publish(&delta.into_event(revision)),
                Err(err) => {
                    // The transcript and the map already changed in memory and
                    // are persisted by the next commit; clients resync on the
                    // revision gap.
                    eprintln!("test runs> failed to record a card change: {err:#}");
                }
            }
        }
    }
}
