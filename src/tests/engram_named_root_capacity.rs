// Owns the regressions for the named-root capacity rules
// (src/engram_named_root_capacity.rs): a root whose claim Engram shows
// closed is retired canonically, a live claim's root never is; legacy and
// claim-refused roots are disposed of by a recorded automatic rule; a naming
// session releases its own root mid-turn; and a release under a running pass
// or a stopped writer waits a bounded time or answers that nothing was
// applied, never an unknown-outcome 500.
// Does not own the status view or the operator release basics
// (engram_named_root_release.rs in this directory) or the automatic pass's
// canonical recovery (engram_named_root_reclamation_*.rs), whose
// `settled_root` fixture this child module reuses.
// New module.
use super::*;

fn retain(claimed: &ClaimedRoot, root: &EngramWorkSourceRoot) {
    claimed.state.inner.lock().unwrap().engram_work_source_roots.push(root.clone());
}

fn retained(claimed: &ClaimedRoot, root: &EngramWorkSourceRoot) -> bool {
    claimed.state.inner.lock().unwrap().engram_work_source_roots.contains(root)
}

fn audit(claimed: &ClaimedRoot) -> Vec<EngramNamedRootReleaseAudit> {
    claimed.state.inner.lock().unwrap().engram_named_root_release_audit.clone()
}

/// A retained root with no canonical run association, named by `named_by`.
fn legacy_of(entry: &EngramWorkSourceRoot, label: &str, named_by: &str, offset: u64) -> EngramWorkSourceRoot {
    let mut legacy = entry.clone();
    legacy.work_id = format!("legacy-work-{label}");
    legacy.claim_id = format!("legacy-claim-{label}");
    legacy.short_ref = format!("w-legacy-{label}");
    legacy.named_by_session = named_by.to_owned();
    legacy.generation = entry.generation + offset;
    legacy
}

fn release_request(root: &EngramWorkSourceRoot, reason: &str) -> EngramNamedRootReleaseRequest {
    EngramNamedRootReleaseRequest {
        project_id: root.store.project_id.clone(),
        database_path: root.store.database_path.to_string_lossy().into_owned(),
        work_id: root.work_id.clone(),
        claim_id: root.claim_id.clone(),
        generation: root.generation,
        root: root.root.clone(),
        reason: reason.to_owned(),
        requested_by: None,
    }
}

fn set_session_in_turn(claimed: &ClaimedRoot, in_turn: bool) {
    claimed.record(|record| {
        record.session.status = if in_turn { SessionStatus::Active } else { SessionStatus::Idle };
        record.engram.active_grant_id = None;
        record.engram.uncertain_grant_id = None;
        record.engram.checkpoint_in_progress = false;
    });
}

/// The canonical read the fixture serves for `entry`'s claim.
fn served_read(claimed: &ClaimedRoot, entry: &EngramWorkSourceRoot) -> Value {
    claimed.transport.named_roots.lock().unwrap().as_ref().unwrap()
        .root_reads.get(&entry.claim_id).cloned().expect("a served read")
}

fn serve_read(claimed: &ClaimedRoot, entry: &EngramWorkSourceRoot, read: Value) {
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
        .root_reads.insert(entry.claim_id.clone(), read);
}

/// The live proof of `entry` with `claim` as Engram's claim block.
fn proof_with_claim(claimed: &ClaimedRoot, entry: &EngramWorkSourceRoot, claim: Value) -> EngramNamedRootReadResponse {
    let mut read = served_read(claimed, entry);
    read["claim"] = claim;
    serde_json::from_value(read).expect("a valid read")
}

fn claim_block(state: &str, fence: i64, expires_at: chrono::DateTime<chrono::Utc>) -> Value {
    json!({"state": state, "holder": "greg/claude",
        "expires_at": expires_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "revision": 3, "fence": fence})
}

// C1: a root whose claim Engram shows closed is retired; a live claim's never.

#[test]
fn a_released_or_completed_claim_retires_its_root() {
    let (claimed, entry) = settled_root("capacity-claim-closed", "live");
    let now = chrono::Utc::now();
    let later = now + chrono::Duration::hours(1);
    for state in ["released", "completed"] {
        let proof = proof_with_claim(&claimed, &entry, claim_block(state, entry.claim_fence, later));
        assert!(
            engram_root_claim_retirement_reason(&entry, &proof, now, true, false).is_some(),
            "a {state} claim's root is retired"
        );
    }
}

#[test]
fn a_claim_recovered_under_a_later_fence_keeps_its_root() {
    // A holder that recovers its own lapsed claim gets a later fence under the
    // same claim, and a same-generation re-name keeps the recorded fence: a
    // later fence is not closure.
    let (claimed, entry) = settled_root("capacity-claim-fenced", "live");
    let now = chrono::Utc::now();
    let proof = proof_with_claim(
        &claimed,
        &entry,
        claim_block("active", entry.claim_fence + 1, now + chrono::Duration::hours(1)),
    );
    assert!(engram_root_claim_retirement_reason(&entry, &proof, now, true, false).is_none());
    assert!(engram_root_claim_retirement_reason(&entry, &proof, now, false, false).is_none());
}

#[test]
fn a_held_claims_lapse_never_retires_its_root() {
    let (claimed, entry) = settled_root("capacity-claim-held-lapse", "live");
    let now = chrono::Utc::now();
    let lapsed = proof_with_claim(
        &claimed,
        &entry,
        claim_block("active", entry.claim_fence + 1, now - chrono::Duration::hours(3)),
    );
    for present in [true, false] {
        assert!(
            engram_root_claim_retirement_reason(&entry, &lapsed, now, present, true).is_none(),
            "a held claim's holder may still recover it (naming session present: {present})"
        );
    }
    let released = proof_with_claim(
        &claimed,
        &entry,
        claim_block("released", entry.claim_fence, now + chrono::Duration::hours(1)),
    );
    assert!(
        engram_root_claim_retirement_reason(&entry, &released, now, true, true).is_some(),
        "a released claim is closed even while a binding still names it"
    );
}

#[test]
fn the_automatic_pass_keeps_a_bound_root_whose_claim_was_recovered_after_a_long_lapse() {
    // Name at fence N, let the claim lapse, recover it at N + 1 and re-name the
    // same generation: the session holds the binding again, so the pass keeps
    // the root however long the claim had lapsed.
    // The idle holder is the root's current owner, so the pass reconciles it
    // on the Current route (the Unfocused route refuses a bound root before
    // any read).
    let (claimed, entry) = settled_root("capacity-claim-recovered-bound", "live");
    let binding = engram_named_root_association(&claimed.state.inner.lock().unwrap(), &entry)
        .cloned()
        .expect("the root's run binding");
    claimed.record(|record| {
        record.engram.work_binding = Some(binding.clone());
        record.session.status = SessionStatus::Idle;
    });
    let mut read = served_read(&claimed, &entry);
    read["claim"] = claim_block(
        "active",
        entry.claim_fence + 1,
        chrono::Utc::now() - chrono::Duration::hours(3),
    );
    serve_read(&claimed, &entry, read);
    let route = EngramRootReclamationRoute::Current(claimed.session_id.clone());
    assert!(
        engram_root_reclamation_route_current(&claimed.state.inner.lock().unwrap(), &entry, &route),
        "the idle holder's route is the one the pass would take"
    );
    let deadline = claimed.state.engram_budget_clock().now() + ENGRAM_ROOT_RECLAMATION_BUDGET;
    // Every refusal before the canonical read is an error; Ok means the read
    // was made and its proof kept the root.
    let retired = claimed
        .state
        .recover_one_engram_root_until(&entry, &route, deadline)
        .expect("the canonical read succeeds");
    assert!(!retired, "a bound root is never retired for its claim's lapse or fence");
    assert!(retained(&claimed, &entry));
}

#[test]
fn a_root_whose_naming_session_was_kept_aside_survives_a_long_claim_lapse() {
    // A naming session kept aside at restore may still hold the claim, but
    // its binding cannot be read: the pass must not take a long lapse as
    // closure, and its claim journal stays.
    let (claimed, entry) = settled_root("capacity-claim-kept-aside", "live");
    let mut kept = entry.clone();
    kept.named_by_session = "session-kept-aside".to_owned();
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.engram_work_source_roots.iter().position(|root| *root == entry).unwrap();
        inner.engram_work_source_roots[index] = kept.clone();
        inner.quarantined_persisted_session_ids.insert("session-kept-aside".to_owned());
    }
    let mut read = served_read(&claimed, &kept);
    read["claim"] = claim_block(
        "active",
        kept.claim_fence,
        chrono::Utc::now() - chrono::Duration::hours(3),
    );
    serve_read(&claimed, &kept, read);
    let deadline = claimed.state.engram_budget_clock().now() + ENGRAM_ROOT_RECLAMATION_BUDGET;
    let retired = claimed
        .state
        .recover_one_engram_root_until(&kept, &EngramRootReclamationRoute::Unfocused, deadline)
        .expect("the canonical read succeeds");
    assert!(!retired, "a kept-aside naming session's root is not retired for a lapse");
    assert!(retained(&claimed, &kept));
    let inner = claimed.state.inner.lock().unwrap();
    let journal = engram_root_journal(&inner.engram_named_root_journal, &kept.store, &kept.claim_id)
        .expect("its claim journal stays");
    assert!(!journal.obsolete, "{journal:#?}");
}

#[test]
fn a_lapsed_claim_keeps_its_root_for_two_hours_while_its_naming_session_is_present() {
    let (claimed, entry) = settled_root("capacity-claim-lapsed", "live");
    let now = chrono::Utc::now();
    let recent = proof_with_claim(
        &claimed,
        &entry,
        claim_block("active", entry.claim_fence, now - chrono::Duration::minutes(30)),
    );
    assert!(
        engram_root_claim_retirement_reason(&entry, &recent, now, true, false).is_none(),
        "a recent lapse keeps the root while its naming session is present"
    );
    assert!(
        engram_root_claim_retirement_reason(&entry, &recent, now, false, false).is_some(),
        "any lapse retires the root once its naming session is gone"
    );
    let old = proof_with_claim(
        &claimed,
        &entry,
        claim_block("active", entry.claim_fence, now - chrono::Duration::hours(3)),
    );
    assert!(
        engram_root_claim_retirement_reason(&entry, &old, now, true, false).is_some(),
        "a lapse older than the grace retires the root"
    );
    assert_eq!(ENGRAM_ROOT_CLAIM_LAPSE_GRACE, Duration::from_secs(2 * 60 * 60));
}

#[test]
fn a_live_claim_or_a_read_without_a_claim_block_keeps_its_root() {
    let (claimed, entry) = settled_root("capacity-claim-live", "live");
    let now = chrono::Utc::now();
    let live = proof_with_claim(
        &claimed,
        &entry,
        claim_block("active", entry.claim_fence, now + chrono::Duration::hours(1)),
    );
    assert!(engram_root_claim_retirement_reason(&entry, &live, now, false, false).is_none());
    let older_engram: EngramNamedRootReadResponse =
        serde_json::from_value(served_read(&claimed, &entry)).unwrap();
    assert!(older_engram.claim.is_none());
    assert!(engram_root_claim_retirement_reason(&entry, &older_engram, now, false, false).is_none());
}

#[test]
fn the_automatic_pass_retires_a_root_whose_claim_was_released() {
    let (claimed, entry) = settled_root("capacity-claim-pass", "live");
    let mut read = served_read(&claimed, &entry);
    read["claim"] = claim_block(
        "released",
        entry.claim_fence,
        chrono::Utc::now() + chrono::Duration::hours(1),
    );
    serve_read(&claimed, &entry, read);
    let deadline = claimed.state.engram_budget_clock().now() + ENGRAM_ROOT_RECLAMATION_BUDGET;
    let retired = claimed
        .state
        .recover_one_engram_root_until(&entry, &EngramRootReclamationRoute::Unfocused, deadline)
        .expect("the canonical read succeeds");
    assert!(retired, "the released claim's root is retired");
    assert!(!retained(&claimed, &entry));
    // Engram still shows the binding Bound, yet the closed claim's journal
    // must not outlive its root: compaction drops it.
    assert_closed_claim_journal_compacts(&claimed, &entry, EngramRootRetirement::Unbound);
}

/// `root`'s claim journal is still kept (neither path compacts after the
/// removal), retired as `retirement`, and the next compaction drops it. The
/// kind matters while something else keeps the journal: Displaced, unlike
/// Unbound, makes an evaluation under that claim stale.
fn assert_closed_claim_journal_compacts(
    claimed: &ClaimedRoot,
    root: &EngramWorkSourceRoot,
    retirement: EngramRootRetirement,
) {
    let mut inner = claimed.state.inner.lock().unwrap();
    let journal = engram_root_journal(&inner.engram_named_root_journal, &root.store, &root.claim_id)
        .expect("the journal is kept until the next compaction");
    assert!(journal.obsolete, "the closed claim's journal is retired: {journal:#?}");
    assert_eq!(journal.retirement, Some(retirement));
    engram_compact_root_journal(&mut inner);
    assert!(
        engram_root_journal(&inner.engram_named_root_journal, &root.store, &root.claim_id).is_none(),
        "compaction drops the closed claim's journal"
    );
}

#[test]
fn a_claim_refused_roots_disposal_retires_its_claim_journal() {
    let (claimed, entry) = settled_root("capacity-refused-journal", "live");
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        engram_named_root_record_refused_claim_locked(&mut inner, &entry.store, &entry.claim_id);
    }
    claimed.state.run_engram_named_root_disposition_pass(claimed.state.engram_budget_clock().now(), &|root| {
        panic!("a refused claim needs no tracker read: {}", root.short_ref)
    });
    assert!(!retained(&claimed, &entry), "Engram no longer counts its claim as its run's");
    let records = audit(&claimed);
    assert_eq!(records.len(), 1, "{records:#?}");
    assert_eq!(records[0].route, "automatic");
    assert_closed_claim_journal_compacts(&claimed, &entry, EngramRootRetirement::Displaced);
}

#[test]
fn only_the_claim_not_current_refusal_closes_a_claim() {
    assert!(engram_named_root_read_refusal_closes_claim(
        "named_root_read_refused: the claim does not belong to the run"
    ));
    for other in [
        "named_root_read_refused: the run is unknown in this store",
        "named_root_read_refused: the run belongs to another project",
        "claim reader timed out",
    ] {
        assert!(!engram_named_root_read_refusal_closes_claim(other), "{other}");
    }
    assert!(ENGRAM_NAMED_ROOT_CLAIM_NOT_CURRENT.contains("does not belong"));
}

// C2: legacy and claim-refused roots are disposed of by a recorded rule.

#[test]
fn a_legacy_root_is_disposed_of_only_by_a_recorded_rule() {
    let (claimed, entry) = settled_root("capacity-legacy-rules", "completed");
    let present = legacy_of(&entry, "present", &claimed.session_id, 100);
    let gone = legacy_of(&entry, "gone", "session-gone", 101);
    retain(&claimed, &present);
    retain(&claimed, &gone);
    let inner = claimed.state.inner.lock().unwrap();
    assert!(engram_named_root_automatic_disposition(&inner, &present, false, None, false).is_some(),
        "its root path is gone");
    assert!(engram_named_root_automatic_disposition(&inner, &present, true, Some("completed"), false).is_some(),
        "its item is completed");
    assert!(engram_named_root_automatic_disposition(&inner, &present, true, Some("cancelled"), false).is_some(),
        "its item is cancelled");
    assert!(engram_named_root_automatic_disposition(&inner, &gone, true, None, false).is_some(),
        "its naming session is gone and no session holds its claim");
    assert!(engram_named_root_automatic_disposition(&inner, &present, true, Some("open"), false).is_none(),
        "an open item's legacy root with a present naming session and an existing path stays");
    assert!(engram_named_root_automatic_disposition(&inner, &entry, true, None, true).is_some(),
        "Engram refused its claim as no longer its run's");
}

#[test]
fn the_disposition_pass_releases_a_legacy_root_whose_path_is_gone_and_records_it() {
    let (claimed, entry) = settled_root("capacity-legacy-pass", "completed");
    let mut legacy = legacy_of(&entry, "pass", &claimed.session_id, 100);
    legacy.root = claimed.root.join("worktree-removed-long-ago").to_string_lossy().into_owned();
    retain(&claimed, &legacy);
    claimed.state.run_engram_named_root_disposition_pass(claimed.state.engram_budget_clock().now(), &|_| None);
    assert!(!retained(&claimed, &legacy), "the legacy root is released");
    assert!(retained(&claimed, &entry), "the canonical root is not this pass's to dispose of");
    let records = audit(&claimed);
    assert_eq!(records.len(), 1, "{records:#?}");
    assert_eq!(records[0].selection, legacy);
    assert_eq!(records[0].route, "automatic");
    assert!(records[0].reason.contains("no longer exists"), "{records:#?}");
    assert!(records[0].acknowledged, "the writer confirmed it");
}

// C3: a naming session releases its own root, even mid-turn.

#[test]
fn a_naming_session_releases_its_own_root_while_in_a_turn() {
    let (claimed, entry) = settled_root("capacity-own", "completed");
    let own = legacy_of(&entry, "own", &claimed.session_id, 100);
    retain(&claimed, &own);
    set_session_in_turn(&claimed, true);
    let response = claimed
        .state
        .release_own_engram_named_root(
            &claimed.session_id,
            EngramNamedRootOwnReleaseRequest {
                work: own.work_id.clone(),
                generation: own.generation,
                reason: "my old root".to_owned(),
            },
        )
        .expect("the naming session may release its own root mid-turn");
    assert!(response.released);
    assert!(!retained(&claimed, &own));
    let records = audit(&claimed);
    assert_eq!(records.len(), 1, "{records:#?}");
    assert_eq!(records[0].route, "own");
    assert_eq!(records[0].requested_by.as_deref(), Some(claimed.session_id.as_str()));
}

#[test]
fn another_session_cannot_release_a_root_as_its_own() {
    let (claimed, entry) = settled_root("capacity-own-other", "completed");
    let others = legacy_of(&entry, "other", "session-someone-else", 100);
    retain(&claimed, &others);
    let request = || EngramNamedRootOwnReleaseRequest {
        work: others.work_id.clone(),
        generation: others.generation,
        reason: "not mine".to_owned(),
    };
    let error = claimed
        .state
        .release_own_engram_named_root(&claimed.session_id, request())
        .expect_err("only the naming session releases its own root");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("naming session"), "{}", error.message);
    let unknown = claimed
        .state
        .release_own_engram_named_root("session-unknown", request())
        .expect_err("an unknown session releases nothing");
    assert_eq!(unknown.status, StatusCode::NOT_FOUND, "{}", unknown.message);
    assert!(retained(&claimed, &others));
}

#[test]
fn a_root_whose_claim_is_bound_is_never_released_as_ones_own() {
    let label = "capacity-own-live";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let tree = add_claimed_root_worktree(&claimed.root);
    name_root(&claimed, label, Some(&tree), vec![claimed_root_held(label)]).unwrap();
    let entry = claimed.state.inner.lock().unwrap().engram_work_source_roots[0].clone();
    let error = claimed
        .state
        .release_own_engram_named_root(
            &claimed.session_id,
            EngramNamedRootOwnReleaseRequest {
                work: entry.work_id.clone(),
                generation: entry.generation,
                reason: "still working".to_owned(),
            },
        )
        .expect_err("a bound claim's root goes by clearing its name, not a release");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(retained(&claimed, &entry));
}

// Contention: a bounded wait, or a definitive answer that nothing was applied.

#[test]
fn a_release_waits_for_a_running_pass_then_applies() {
    let (claimed, entry) = settled_root("capacity-flight-wait", "completed");
    claimed.state.inner.lock().unwrap().engram_root_reclamation.flight = Some("running-pass".to_owned());
    let waited = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hook_waited = waited.clone();
    let state = claimed.state.clone();
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary =
        Some(Arc::new(move |phase| {
            if phase == "named_root_release_waiting_for_flight"
                && !hook_waited.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                // The pass finishes while the release waits.
                state.inner.lock().unwrap().engram_root_reclamation.flight = None;
            }
        }));
    let response = claimed
        .state
        .release_engram_named_root(Some("session-operator"), release_request(&entry, "item completed"));
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;
    assert!(waited.load(std::sync::atomic::Ordering::SeqCst), "the release waited for the pass");
    assert!(response.expect("applied after the pass").released);
    assert!(!retained(&claimed, &entry));
}

#[test]
fn a_release_that_outwaits_a_running_pass_answers_that_nothing_was_applied() {
    let (claimed, entry) = settled_root("capacity-flight-busy", "completed");
    claimed.state.inner.lock().unwrap().engram_root_reclamation.flight = Some("running-pass".to_owned());
    let clock = claimed.state.engram_budget_clock();
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary =
        Some(Arc::new(move |phase| {
            if phase == "named_root_release_waiting_for_flight" {
                clock.advance(ENGRAM_NAMED_ROOT_RELEASE_FLIGHT_WAIT + Duration::from_secs(1));
            }
        }));
    let result = claimed
        .state
        .release_engram_named_root(Some("session-operator"), release_request(&entry, "item completed"));
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;
    let error = result.expect_err("the pass outlasted the wait");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("nothing was applied"), "{}", error.message);
    assert!(retained(&claimed, &entry));
    assert!(audit(&claimed).is_empty());
}

#[test]
fn a_release_failing_before_its_removal_is_a_definitive_conflict_not_a_500() {
    let (claimed, entry) = settled_root("capacity-writer-stopped", "completed");
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(false));
    let result = claimed
        .state
        .release_engram_named_root(Some("session-operator"), release_request(&entry, "item completed"));
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(true));
    let error = result.expect_err("a stopped writer applies nothing");
    assert_eq!(error.status, StatusCode::CONFLICT, "{}", error.message);
    assert!(error.message.contains("not applied"), "{}", error.message);
    assert!(retained(&claimed, &entry));
    assert!(audit(&claimed).is_empty());
}

#[test]
fn a_release_has_its_own_authority_budget_beyond_the_passes() {
    let (claimed, entry) = settled_root("capacity-release-budget", "completed");
    let clock = claimed.state.engram_budget_clock();
    let slowed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hook_slowed = slowed.clone();
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary =
        Some(Arc::new(move |phase| {
            if phase == "before_publication"
                && !hook_slowed.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                // A busy writer: longer than the pass's budget, within the release's.
                clock.advance(ENGRAM_ROOT_RECLAMATION_BUDGET + Duration::from_secs(1));
            }
        }));
    let result = claimed
        .state
        .release_engram_named_root(Some("session-operator"), release_request(&entry, "item completed"));
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;
    assert!(slowed.load(std::sync::atomic::Ordering::SeqCst));
    assert!(ENGRAM_NAMED_ROOT_RELEASE_AUTHORITY_BUDGET > ENGRAM_ROOT_RECLAMATION_BUDGET + Duration::from_secs(1));
    assert!(result.expect("the release's budget covers a busy writer").released);
    assert!(!retained(&claimed, &entry));
}

#[test]
fn a_release_body_error_names_every_missing_field() {
    let missing = engram_named_root_release_missing_fields(&json!({"projectId": "p"}));
    assert_eq!(
        missing,
        ["databasePath", "workId", "claimId", "generation", "root", "reason"],
    );
    let wrong_type = engram_named_root_release_missing_fields(&json!({
        "projectId": "p", "databasePath": "d", "workId": "w", "claimId": "c",
        "generation": "seven", "root": "r", "reason": "x"
    }));
    assert_eq!(wrong_type, ["generation"]);
}

// The disposition is scheduled on its own, reads the item's tracker
// lifecycle in production, and recovers a release whose confirmation failed.

/// Installs `read` as this thread's item tracker read until dropped.
struct ScriptedItemLifecycle;

impl ScriptedItemLifecycle {
    fn install(read: impl Fn(&EngramWorkSourceRoot) -> Option<String> + 'static) -> Self {
        TEST_ENGRAM_NAMED_ROOT_ITEM_LIFECYCLE.with(|hook| *hook.borrow_mut() = Some(Box::new(read)));
        Self
    }
}

impl Drop for ScriptedItemLifecycle {
    fn drop(&mut self) {
        TEST_ENGRAM_NAMED_ROOT_ITEM_LIFECYCLE.with(|hook| *hook.borrow_mut() = None);
    }
}

#[test]
fn the_scheduler_runs_a_disposition_pass_for_a_table_of_legacy_roots_alone() {
    let (claimed, entry) = settled_root("capacity-legacy-only", "completed");
    let mut gone = legacy_of(&entry, "only", &claimed.session_id, 100);
    gone.root = claimed.root.join("worktree-removed-only").to_string_lossy().into_owned();
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        inner.engram_work_source_roots.clear();
        inner.engram_work_source_roots.push(gone.clone());
    }
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary =
        Some(Arc::new(move |phase| {
            if phase == "after_reclamation_pass" {
                let _ = done_tx.send(());
            }
        }));
    let scheduled = claimed.state.schedule_engram_root_reclamation(true);
    assert_eq!(scheduled, "a bounded reclamation pass was scheduled", "no canonical read is needed");
    done_rx.recv_timeout(Duration::from_secs(30)).expect("the pass ran");
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;
    assert!(!retained(&claimed, &gone), "the legacy root is released without any canonical read");
    let records = audit(&claimed);
    assert_eq!(records.len(), 1, "{records:#?}");
    assert_eq!(records[0].route, "automatic");
    assert_eq!(
        claimed.state.schedule_engram_root_reclamation(true),
        "no eligible reclamation pass was scheduled",
        "nothing is due once it is done"
    );
}

#[test]
fn the_reclamation_pass_reads_a_legacy_roots_item_lifecycle() {
    let (claimed, entry) = settled_root("capacity-item-lifecycle", "completed");
    let completed = legacy_of(&entry, "item-completed", &claimed.session_id, 100);
    let open = legacy_of(&entry, "item-open", &claimed.session_id, 101);
    retain(&claimed, &completed);
    retain(&claimed, &open);
    let completed_work = completed.work_id.clone();
    let _lifecycle = ScriptedItemLifecycle::install(move |root| {
        Some(if root.work_id == completed_work { "completed" } else { "open" }.to_owned())
    });
    claimed.state.run_engram_root_reclamation_pass(Vec::new());
    assert!(!retained(&claimed, &completed), "a completed item's legacy root is released");
    assert!(retained(&claimed, &open), "an open item's legacy root stays");
    let records = audit(&claimed);
    assert_eq!(records.len(), 1, "{records:#?}");
    assert_eq!(records[0].selection, completed);
    assert!(records[0].reason.contains("completed"), "{records:#?}");
    assert!(
        claimed.state.inner.lock().unwrap().engram_root_reclamation.disposition_cooldowns
            .iter().any(|(cooling, _)| *cooling == open),
        "the kept root is left alone for a while"
    );
}

#[test]
fn a_root_no_disposition_may_touch_costs_no_tracker_read() {
    let (claimed, entry) = settled_root("capacity-blocked-no-read", "completed");
    let kept_aside = legacy_of(&entry, "kept-aside", "session-kept-aside", 100);
    retain(&claimed, &kept_aside);
    claimed
        .state
        .inner
        .lock()
        .unwrap()
        .quarantined_persisted_session_ids
        .insert("session-kept-aside".to_owned());
    let now = claimed.state.engram_budget_clock().now();
    assert!(
        !engram_named_root_disposition_due_locked(&claimed.state.inner.lock().unwrap(), now),
        "a blocked root alone makes no pass due"
    );
    claimed.state.run_engram_named_root_disposition_pass(claimed.state.engram_budget_clock().now(), &|root| {
        panic!("no tracker read for a blocked root: {}", root.short_ref)
    });
    assert!(retained(&claimed, &kept_aside), "a root whose naming session was kept aside stays");
    assert!(audit(&claimed).is_empty());
}

#[test]
fn the_item_lifecycle_is_read_through_the_work_host_reader() {
    let (state, project, _, root) = super::super::work_visualizer::fixture();
    super::super::work_visualizer::install_store(&state, &project, &root);
    let store = state
        .inner
        .lock()
        .unwrap()
        .projects
        .iter()
        .find(|candidate| candidate.id == project)
        .and_then(|candidate| candidate.engram.as_ref())
        .and_then(|settings| settings.authority_store_key.clone())
        .expect("the established store");
    let (_template_owner, template) = settled_root("capacity-item-reader", "completed");
    let mut named = template.clone();
    named.store = store;
    named.work_id = "fixture-work-id".to_owned();
    assert_eq!(state.read_engram_named_root_item_lifecycle(&named).as_deref(), Some("open"));
    let args = fs::read_to_string(root.join("work-read-args.txt")).expect("the reader ran");
    let args = args.lines().collect::<Vec<_>>();
    assert!(args.contains(&"show") && args.contains(&"fixture-work-id") && args.contains(&"--json"), "{args:?}");
    let mut elsewhere = named.clone();
    elsewhere.store.project_id = "a-store-no-project-establishes".to_owned();
    assert_eq!(state.read_engram_named_root_item_lifecycle(&elsewhere), None, "no reader, no answer");
}

#[test]
fn a_later_pass_confirms_an_automatic_release_whose_confirmation_failed() {
    let (claimed, entry) = settled_root("capacity-pending-automatic", "completed");
    let mut gone = legacy_of(&entry, "pending", &claimed.session_id, 100);
    gone.root = claimed.root.join("worktree-removed-pending").to_string_lossy().into_owned();
    retain(&claimed, &gone);
    TEST_ENGRAM_NAMED_ROOT_RELEASE_UNCONFIRMED.with(|unconfirmed| unconfirmed.set(true));
    claimed.state.run_engram_named_root_disposition_pass(claimed.state.engram_budget_clock().now(), &|_| None);
    TEST_ENGRAM_NAMED_ROOT_RELEASE_UNCONFIRMED.with(|unconfirmed| unconfirmed.set(false));
    assert!(!retained(&claimed, &gone));
    let records = audit(&claimed);
    assert_eq!(records.len(), 1, "{records:#?}");
    assert!(!records[0].acknowledged, "the writer did not confirm it");
    assert_eq!(
        engram_named_root_unacknowledged_releases(&claimed.state.inner.lock().unwrap()),
        1,
        "its slot stays reserved meanwhile"
    );
    // Read the clock before taking the state lock: the test budget clock
    // takes that lock itself, and the lock is not reentrant.
    let now = claimed.state.engram_budget_clock().now();
    assert!(
        engram_named_root_disposition_due_locked(&claimed.state.inner.lock().unwrap(), now),
        "a pending release makes a pass due"
    );

    claimed.state.run_engram_named_root_disposition_pass(claimed.state.engram_budget_clock().now(), &|_| None);
    let records = audit(&claimed);
    assert_eq!(records.len(), 1, "{records:#?}");
    assert!(records[0].acknowledged, "a later pass confirmed it without a restart");
    assert_eq!(engram_named_root_unacknowledged_releases(&claimed.state.inner.lock().unwrap()), 0);
}
