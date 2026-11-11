// Named-root capacity witnesses through existing canonical recovery and naming.
// Fixtures retain the original run association after the naming session moves on.
use super::*;

mod recovery {
    include!("engram_named_root_reclamation_recovery.rs");
}

mod safety {
    include!("engram_named_root_reclamation_safety.rs");
}

mod reader_authority {
    include!("engram_named_root_reclamation_readers.rs");
}

mod eligibility {
    include!("engram_named_root_reclamation_eligibility.rs");
}

mod recovery_lock {
    include!("engram_root_recovery_lock.rs");
}

mod scheduler {
    include!("engram_named_root_reclamation_scheduler.rs");
}

mod retry {
    include!("engram_named_root_reclamation_retry.rs");
}

mod deferral {
    use super::*;
    include!("engram_named_root_reclamation_deferral.rs");
}

mod release {
    include!("engram_named_root_release.rs");
}

mod capacity {
    include!("engram_named_root_capacity.rs");
}

mod completion_clear {
    include!("engram_named_root_completion_clear.rs");
}


// Compose the existing EO state/work-service owner with a real Git worktree.
pub(super) fn root_fixture(label: &str) -> (EOCheckedTurn, PathBuf) {
    let turn = EOCheckedTurn::idle(label, true);
    let path = turn.root.join(".worktrees").join("retained-root");
    run_git_test_command(&turn.root, &[
        "worktree", "add", "--quiet", "-b", "retained-root", path.to_str().unwrap(),
    ]);
    (turn, path)
}

pub(super) fn request_fixture_name(turn: &EOCheckedTurn, path: Option<&FsPath>)
    -> Result<EngramSourceRootResponse, ApiError>
{
    let work = turn.record(|record| record.engram.work_binding.as_ref().unwrap().work_id.clone());
    assert_ordinary_turn_has_no_native_requests(turn);
    let result = turn.state.name_engram_source_root(&turn.session_id, EngramSourceRootRequest {
        work, path: path.map(|path| Some(path.to_string_lossy().into_owned())),
    });
    assert_ordinary_turn_has_no_native_requests(turn);
    result
}

fn retained_root_proof(journal: &EngramNamedRootJournal, released: bool) -> Value {
    let binding = journal.read_binding.as_ref().unwrap();
    let (event, receipt) = journal.confirmed.as_ref().unwrap();
    let position = json!({"feed": {"kind": "run_execution", "id": binding.run_id},
        "position": receipt.position.position});
    json!({
        "project_id": journal.store.project_id, "work_id": binding.work_id,
        "root_execution_id": binding.root_execution_id, "run_id": binding.run_id,
        "claim_id": binding.claim_id, "run": {"state": if released {"open"} else {"completed"}, "generation":1},
        "named_root": if released {
            json!({"state": "unbound_by_release", "last_generation": receipt.generation,
                "released_at_position": receipt.position.position + 1})
        } else { json!({"state": "none"}) },
        "latest_event": {"event": receipt.event, "position": position,
            "generation": receipt.generation, "kind": receipt.kind,
            "workspace_id": receipt.workspace_id, "named_at": event.root.named_at},
        "read_cut": {"feed": {"kind": "run_execution", "id": binding.run_id},
            "position": receipt.position.position + 1}
    })
}

fn settled_root(label: &str, proof_kind: &str) -> (EOCheckedTurn, EngramWorkSourceRoot) {
    let (claimed, tree) = root_fixture(label);
    request_fixture_name(&claimed, Some(&tree)).unwrap();
    let (entry, journal) = {
        let inner = claimed.state.inner.lock().unwrap();
        let entry = inner.engram_work_source_roots[0].clone();
        assert!(!engram_authority_work_unresolved(&inner, &entry.store, &entry.work_id));
        let journal = engram_root_journal(
            &inner.engram_named_root_journal, &entry.store, &entry.claim_id,
        ).unwrap().clone();
        assert!(journal.pending.is_none());
        assert!(journal.read_binding.is_some());
        (entry, journal)
    };
    let mut proof = retained_root_proof(&journal, proof_kind == "released");
    match proof_kind {
        "completed" | "released" => {}
        "cancelled" => proof["run"]["state"] = json!("cancelled"),
        "ended" => {
            proof["run"]["state"] = json!("active");
            proof["latest_event"]["kind"] = json!("ended");
            proof["latest_event"]["event"] = json!("confirmed-ended-event");
            proof["latest_event"]["position"]["position"] = proof["read_cut"]["position"].clone();
        }
        "live" => {
            proof["run"]["state"] = json!("active");
            proof["named_root"] = json!({"state":"bound", "workspace_id":entry.root,
                "generation":entry.generation, "named_at":entry.named_at});
        }
        "unknown" => proof["named_root"] = json!({"state":"unknown"}),
        _ => unreachable!(),
    }
    if proof_kind != "unknown" {
        serde_json::from_value::<EngramNamedRootReadResponse>(proof.clone()).unwrap()
            .validate_authority(&entry.store, journal.read_binding.as_ref().unwrap()).unwrap();
    }
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
        .root_reads.insert(entry.claim_id.clone(), proof);
    let mut successor = journal.read_binding.unwrap();
    successor.work_id.push_str("-next");
    successor.claim_id.push_str("-next");
    successor.run_id.push_str("-next");
    successor.root_execution_id.push_str("-next");
    claimed.record_mut(|record| {
        record.engram.work_binding = Some(successor.clone());
        record.engram.named_root = Some(EngramNamedRootState::None);
    });
    claimed.transport.enable_named_roots(&claimed.session_id, &successor);
    claimed.transport.replace_held_claims([Ok(EngramHeldClaims { items: Vec::new(), omitted: 0 })]);
    (claimed, entry)
}

fn recovery_frees_capacity(proof_kind: &str) {
    let label = format!("root-reclamation-{proof_kind}");
    let (claimed, entry) = settled_root(&label, proof_kind);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        for number in 1..ENGRAM_WORK_SOURCE_ROOT_LIMIT {
            let mut retained = entry.clone();
            retained.work_id = format!("retained-work-{number}");
            retained.claim_id = format!("retained-claim-{number}");
            retained.short_ref = format!("w-retained-{number}");
            inner.engram_work_source_roots.push(retained);
        }
        assert_eq!(inner.engram_work_source_roots.len(), 64);
    }
    claimed.state.recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    let mut inner = claimed.state.inner.lock().unwrap();
    assert!(engram_work_source_root_for_claim(&inner.engram_work_source_roots,
        &entry.store, &entry.work_id, &entry.claim_id).is_none(),
        "a canonically ended old claim must not retain a capacity slot after focus moved");
    assert_eq!(inner.engram_work_source_roots.len(), 63,
        "unknown associations must survive; only the proven old entry retires");
    let mut replacement = entry.clone();
    replacement.work_id = "new-work-after-reclamation".to_owned();
    replacement.claim_id = "new-claim-after-reclamation".to_owned();
    replacement.generation += 1;
    engram_set_work_source_root(&mut inner.engram_work_source_roots,
        &entry.store, &replacement.work_id.clone(), Some(replacement)).unwrap();
    assert_eq!(inner.engram_work_source_roots.len(), 64);
}

/// A settled root, whose session's focus has moved, with its run still live.
fn live_unfocused_root(label: &str) -> (EOCheckedTurn, EngramWorkSourceRoot, EngramAuthorityTransition) {
    let (claimed, entry) = settled_root(label, "live");
    let owner = authority_owner(&claimed, &entry);
    assert_eq!(owner.phase, EngramAuthorityPhase::Published);
    (claimed, entry, owner)
}

fn authority_owner(claimed: &EOCheckedTurn, entry: &EngramWorkSourceRoot) -> EngramAuthorityTransition {
    let inner = claimed.state.inner.lock().unwrap();
    inner.engram_work_naming_history.iter()
        .find(|history| history.store == entry.store && history.work_id == entry.work_id).unwrap()
        .transition.clone().unwrap()
}

fn unresolved_reason(claimed: &EOCheckedTurn, entry: &EngramWorkSourceRoot) -> Option<EngramUnconfirmedRootReason> {
    let inner = claimed.state.inner.lock().unwrap();
    engram_authority_work_unresolved_reason(&inner, &entry.store, &entry.work_id)
}

fn root_reads(claimed: &EOCheckedTurn) -> usize {
    claimed.transport.requests().iter()
        .filter(|request| request.request["operation"] == "named_root_read").count()
}

/// The next publication at `phase` finds its budget spent, as a slow writer
/// leaves it, on the fixture's scripted clock. Time isn't measured.
fn refuse_next_publication(claimed: &EOCheckedTurn, phase: &'static str) -> Arc<std::sync::atomic::AtomicBool> {
    let clock = claimed.state.engram_budget_clock();
    let refused = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = refused.clone();
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = Some(Arc::new(move |at| {
        if at == phase && !observed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            clock.advance(ENGRAM_MIN_CALL_TIMEOUT + Duration::from_millis(1));
        }
    }));
    refused
}

/// Another session's maintenance reads a settled root's run, and has nothing to
/// settle on it, so that work's authority doesn't move at all.
#[test]
fn a_maintenance_pass_reads_a_settled_root_and_leaves_its_authority_alone() {
    let (claimed, entry, owner) = live_unfocused_root("root-settled-read");
    let reads = root_reads(&claimed);
    claimed.state.recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    assert!(root_reads(&claimed) > reads, "the lifecycle read still happens");
    assert_eq!(authority_owner(&claimed, &entry), owner, "no owner was prepared for nothing");
    assert_eq!(unresolved_reason(&claimed, &entry), None);
}

/// Before the pass read first, a refused publication here stranded the work as a
/// Candidate, and its session's evidence was withheld over nothing.
#[test]
fn a_maintenance_pass_cannot_strand_a_settled_root_when_a_publication_is_refused() {
    let (claimed, entry, owner) = live_unfocused_root("root-settled-refused");
    let refused = refuse_next_publication(&claimed, "before_publication");
    claimed.state.recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    assert!(!refused.load(std::sync::atomic::Ordering::SeqCst), "nothing was left to publish");
    assert_eq!(authority_owner(&claimed, &entry), owner);
    assert_eq!(unresolved_reason(&claimed, &entry), None);
}

/// Another session's maintenance over the same root, as a second reader.
#[test]
fn another_sessions_maintenance_pass_leaves_a_settled_root_published() {
    let (claimed, entry, owner) = live_unfocused_root("root-settled-other");
    let other = {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        let (workdir, project) = (
            inner.sessions[index].session.workdir.clone(),
            inner.sessions[index].session.project_id.clone(),
        );
        inner.create_session(Agent::Codex, Some("Other session".to_owned()), workdir, project, None)
            .session.id
    };
    let refused = refuse_next_publication(&claimed, "before_publication");
    let reads = root_reads(&claimed);
    claimed.state.recover_engram_authority_runs(&other, Duration::from_secs(2));
    assert!(root_reads(&claimed) > reads, "the other session did read it");
    assert!(!refused.load(std::sync::atomic::Ordering::SeqCst), "nothing was left to publish");
    assert_eq!(authority_owner(&claimed, &entry), owner);
    assert_eq!(unresolved_reason(&claimed, &entry), None);
}

/// An unanswered read learns nothing, and the root stays as it was.
#[test]
fn an_unanswered_read_leaves_a_settled_root_published() {
    let (claimed, entry, owner) = live_unfocused_root("root-settled-unanswered");
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
        .root_read_errors.insert(entry.claim_id.clone(), json!({"code": "store_busy", "message": "try later"}));
    claimed.state.recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    assert_eq!(authority_owner(&claimed, &entry), owner);
    assert_eq!(unresolved_reason(&claimed, &entry), None);
}

/// A publication interrupted in a real transition is finished in the background,
/// needing no naming: a turn opening asks for this pass.
#[test]
fn an_interrupted_publication_is_finished_by_the_next_background_pass() {
    let (claimed, entry, _) = live_unfocused_root("root-candidate-background");
    let owner = claimed.state.prepare_engram_authority(&entry.store,
        &authority_owner(&claimed, &entry).binding, Duration::from_secs(2)).unwrap();
    let refused = refuse_next_publication(&claimed, "before_publication");
    assert!(claimed.state.publish_engram_authority(&entry.store, &owner, Duration::from_secs(2)).is_err());
    assert!(refused.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(authority_owner(&claimed, &entry).phase, EngramAuthorityPhase::Candidate);
    assert!(unresolved_reason(&claimed, &entry).is_some(), "while it's unfinished, evidence stays withheld");
    {
        let inner = claimed.state.inner.lock().unwrap();
        let history = inner.engram_work_naming_history.iter()
            .find(|history| history.store == entry.store && history.work_id == entry.work_id).unwrap();
        assert!(engram_history_needs_recovery_locked(&inner, history), "a turn opening asks for the pass");
    }
    claimed.state.recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    assert_eq!(authority_owner(&claimed, &entry).phase, EngramAuthorityPhase::Published);
    assert_eq!(unresolved_reason(&claimed, &entry), None);
}

/// A same-path naming returns its root's generation only with the root confirmed,
/// even when the preceding opening pass could not finish its stranded
/// publication. The naming recovers that exact candidate and publishes it;
/// the first naming request still succeeds without a replacement bind.
#[test]
fn a_same_path_naming_clears_a_candidate_its_opening_pass_could_not_finish() {
    let (claimed, tree) = root_fixture("root-candidate-same-path");
    request_fixture_name(&claimed, Some(&tree)).unwrap();
    let entry = claimed.state.inner.lock().unwrap().engram_work_source_roots[0].clone();
    let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    let owner = claimed.state.prepare_engram_authority(&entry.store, &binding, Duration::from_secs(2)).unwrap();
    let refused = refuse_next_publication(&claimed, "before_publication");
    assert!(claimed.state.publish_engram_authority(&entry.store, &owner, Duration::from_secs(2)).is_err());
    assert!(refused.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(authority_owner(&claimed, &entry).phase, EngramAuthorityPhase::Candidate);
    let opening_refused = refuse_next_publication(&claimed, "before_recovery_publication");

    // Naming defers its unfunded optional maintenance. Exercise the actual
    // preceding opening owner explicitly, so its fault cannot instead expire
    // the mandatory recovery in the naming request below.
    claimed.state.recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    assert!(opening_refused.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(authority_owner(&claimed, &entry).phase, EngramAuthorityPhase::Candidate);
    assert!(unresolved_reason(&claimed, &entry).is_some());

    let named = request_fixture_name(&claimed, Some(&tree));

    assert!(opening_refused.load(std::sync::atomic::Ordering::SeqCst), "the opening pass reached publication");
    assert_eq!(named.map(|reply| reply.generation).map_err(|error| error.message), Ok(entry.generation));
    assert_eq!(authority_owner(&claimed, &entry).phase, EngramAuthorityPhase::Published);
    assert_eq!(unresolved_reason(&claimed, &entry), None);
}

/// Runs `action` during the owner's own read in a recurring pass, not during the read
/// that decided to go on: the fences that guard a retirement judge that one.
pub(super) fn during_owners_read(action: impl FnOnce() + 'static) {
    TEST_ENGRAM_DURING_ROOT_LIFECYCLE_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            TEST_ENGRAM_DURING_ROOT_LIFECYCLE_READ.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(action));
            });
        }));
    });
}

/// A live root its idle owner has in focus, confirmed.
fn live_idle_root(label: &str) -> (EOCheckedTurn, EngramWorkSourceRoot, EngramAuthorityTransition) {
    let (claimed, entry, owner) = live_unfocused_root(label);
    claimed.record_mut(|record| {
        record.engram.work_binding = Some(owner.binding.clone());
        record.engram.named_root = Some(EngramNamedRootState::Bound {
            workspace_id: entry.root.clone(),
            generation: entry.generation as i64,
            named_at: entry.named_at.clone(),
        });
        record.session.status = SessionStatus::Idle;
    });
    claimed.transport.enable_named_roots(&claimed.session_id, &owner.binding);
    let capture = root_capture(&claimed);
    assert!(matches!(capture, Ok(EngramRootCapture::Recorded { .. })), "confirmed first: {capture:?}");
    (claimed, entry, owner)
}

/// What a turn of this session opening now would capture.
fn root_capture(claimed: &EOCheckedTurn) -> Result<EngramRootCapture, EngramUnconfirmedRootReason> {
    engram_root_capture_explained_locked(&claimed.state.inner.lock().unwrap(), &claimed.session_id)
}

/// A real transition whose publication was refused, leaving it a Candidate.
fn strand_a_publication(
    claimed: &EOCheckedTurn,
    entry: &EngramWorkSourceRoot,
    owner: &EngramAuthorityTransition,
) -> EngramAuthorityTransition {
    let stranded = claimed.state.prepare_engram_authority(&entry.store, &owner.binding, Duration::from_secs(2)).unwrap();
    let refused = refuse_next_publication(claimed, "before_publication");
    assert!(claimed.state.publish_engram_authority(&entry.store, &stranded, Duration::from_secs(2)).is_err());
    assert!(refused.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(authority_owner(claimed, entry).phase, EngramAuthorityPhase::Candidate);
    stranded
}

/// Finishing the publication needs no read, so one left unanswered doesn't put
/// the work back: no new owner, and no retained run.
#[test]
fn an_interrupted_publication_is_finished_without_an_answered_read() {
    let (claimed, entry, owner) = live_unfocused_root("root-candidate-unanswered");
    let stranded = strand_a_publication(&claimed, &entry, &owner);
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
        .root_read_errors.insert(entry.claim_id.clone(), json!({"code": "store_busy", "message": "try later"}));
    claimed.state.recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    assert_eq!(unresolved_reason(&claimed, &entry), None);
    let finished = authority_owner(&claimed, &entry);
    assert_eq!(finished.phase, EngramAuthorityPhase::Published);
    assert_eq!((finished.id, finished.version), (stranded.id, stranded.version));
}

/// The owner's run and the root's differ, so the pass meets the work twice. Whichever
/// comes first finishes the publication, and the other finds the work settled: no
/// new owner, even with the root's read unanswered.
fn a_publication_on_another_run_is_finished_once(label: &str, root_first: bool) {
    let (claimed, entry, owner) = live_unfocused_root(label);
    let mut later = owner.binding.clone();
    later.run_id.push_str("-later");
    later.claim_id.push_str("-later");
    let planted = {
        let mut inner = claimed.state.inner.lock().unwrap();
        let history = inner.engram_work_naming_history.iter_mut()
            .find(|history| history.store == entry.store && history.work_id == entry.work_id).unwrap();
        history.owner_version += 1;
        let planted = EngramAuthorityTransition {
            id: Uuid::new_v4().to_string(),
            version: history.owner_version,
            binding: later.clone(),
            phase: EngramAuthorityPhase::Candidate,
        };
        history.transition = Some(planted.clone());
        // The pass starts just after the run named here, in a fair rotation.
        let last = if root_first { later.claim_id.clone() } else { entry.claim_id.clone() };
        inner.engram_authority_read_cursor.insert(entry.store.clone(), last);
        planted
    };
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
        .root_read_errors.insert(entry.claim_id.clone(), json!({"code": "store_busy", "message": "try later"}));
    claimed.state.recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    assert_eq!(unresolved_reason(&claimed, &entry), None);
    let finished = authority_owner(&claimed, &entry);
    assert_eq!(finished.phase, EngramAuthorityPhase::Published);
    assert_eq!((finished.id, finished.version), (planted.id, planted.version), "no new owner");
}

#[test]
fn a_publication_on_another_run_is_finished_once_with_the_owners_run_first() {
    a_publication_on_another_run_is_finished_once("root-two-runs-owner-first", false);
}

#[test]
fn a_publication_on_another_run_is_finished_once_with_the_roots_run_first() {
    a_publication_on_another_run_is_finished_once("root-two-runs-root-first", true);
}

/// A retired run is never read again, so a Candidate left on one is only published,
/// even while its root row survives.
#[test]
fn a_candidate_on_a_retired_run_is_finished_without_another_read() {
    let (claimed, entry, owner) = live_unfocused_root("root-candidate-retired");
    let planted = {
        let mut inner = claimed.state.inner.lock().unwrap();
        let history = inner.engram_work_naming_history.iter_mut()
            .find(|history| history.store == entry.store && history.work_id == entry.work_id).unwrap();
        history.retire_recovery_run(&owner.binding, EngramNamedRootReadRefusal::RunUnknown,
            &entry.store, "fixture refusal");
        history.owner_version += 1;
        let planted = EngramAuthorityTransition {
            id: Uuid::new_v4().to_string(),
            version: history.owner_version,
            binding: owner.binding.clone(),
            phase: EngramAuthorityPhase::Candidate,
        };
        history.transition = Some(planted.clone());
        planted
    };
    let reads = root_reads(&claimed);
    claimed.state.recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    assert_eq!(root_reads(&claimed), reads, "a retired run isn't read");
    let finished = authority_owner(&claimed, &entry);
    assert_eq!(finished.phase, EngramAuthorityPhase::Published);
    assert_eq!((finished.id, finished.version), (planted.id, planted.version), "no new owner");
}

/// An answer that contradicts acknowledged history isn't nothing learned: the same
/// bound root, at the same position, under another event's identity. It goes on to
/// the owner, whose learning refuses it and keeps evidence withheld.
#[test]
fn a_read_contradicting_acknowledged_history_keeps_withholding() {
    let (claimed, entry, owner) = live_unfocused_root("root-settled-contradiction");
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
        .root_reads.get_mut(&entry.claim_id).unwrap()["latest_event"]["event"] = json!("another-event");
    claimed.state.recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    assert_eq!(unresolved_reason(&claimed, &entry), Some(EngramUnconfirmedRootReason::WorkRunsUnresolved));
    assert_ne!(authority_owner(&claimed, &entry).id, owner.id, "an owner settles it");
}

/// A turn opening on a stranded publication doesn't wait for it: it stays honestly
/// unconfirmed and keeps that opening for its length, it asks for the background
/// pass, and the next ordinary turn opens confirmed.
#[test]
fn a_turn_opening_on_an_interrupted_publication_stays_unconfirmed_and_the_next_opens_confirmed() {
    let (claimed, entry, owner) = live_idle_root("root-candidate-opening");
    strand_a_publication(&claimed, &entry, &owner);
    let opening = |claimed: &EOCheckedTurn| runtime_observation_opening_root(
        &claimed.state.inner.lock().unwrap(), &claimed.session_id).unwrap().provenance;
    assert_eq!(opening(&claimed), EngramRootCapture::Unconfirmed);
    TEST_ENGRAM_RETAINED_RUN_READS_ENABLED.with(|enabled| enabled.set(true));
    claimed.state.engram_host().turn_starting(&claimed.session_id);
    TEST_ENGRAM_RETAINED_RUN_READS_ENABLED.with(|enabled| enabled.set(false));
    for read in TEST_ENGRAM_RETAINED_RUN_READS.with(|reads| std::mem::take(&mut *reads.borrow_mut())) {
        read.join().expect("the background pass finished");
    }
    assert_eq!(authority_owner(&claimed, &entry).phase, EngramAuthorityPhase::Published);
    let next = opening(&claimed);
    assert!(matches!(next, EngramRootCapture::Recorded { .. }), "{next:?}");
}

/// If the pass can't finish the publication either, evidence stays withheld: an
/// unsettled transition isn't overtaken.
#[test]
fn a_publication_the_pass_cannot_finish_keeps_withholding() {
    let (claimed, entry, owner) = live_idle_root("root-candidate-pass-refused");
    strand_a_publication(&claimed, &entry, &owner);
    let refused = refuse_next_publication(&claimed, "before_recovery_publication");
    claimed.state.recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    assert!(refused.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(root_capture(&claimed), Err(EngramUnconfirmedRootReason::WorkTransitionUnpublished));
    assert_eq!(authority_owner(&claimed, &entry).phase, EngramAuthorityPhase::Candidate);
}

/// A Prepared owner with no retained run belongs to its naming, and a turn opening
/// doesn't start a pass over it.
#[test]
fn a_prepared_owner_with_nothing_retained_asks_for_no_pass() {
    let (claimed, entry, owner) = live_idle_root("root-prepared-no-pass");
    claimed.state.prepare_engram_authority(&entry.store, &owner.binding, Duration::from_secs(2)).unwrap();
    let mut inner = claimed.state.inner.lock().unwrap();
    let index = inner.engram_work_naming_history.iter()
        .position(|history| history.store == entry.store && history.work_id == entry.work_id).unwrap();
    inner.engram_work_naming_history[index].unresolved_runs.clear();
    let history = inner.engram_work_naming_history[index].clone();
    assert_eq!(history.transition.as_ref().unwrap().phase, EngramAuthorityPhase::Prepared);
    assert!(!engram_history_needs_recovery_locked(&inner, &history));
}

/// A bound read can still retire its root: the retirement rules decide, so a
/// released claim isn't passed over as nothing learned.
#[test]
fn a_bound_read_of_a_released_claim_still_retires_its_root() {
    let (claimed, entry, _) = live_unfocused_root("root-settled-released-claim");
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
        .root_reads.get_mut(&entry.claim_id).unwrap()["claim"] = json!({"state": "released"});
    claimed.state.recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    assert!(!claimed.state.inner.lock().unwrap().engram_work_source_roots.contains(&entry),
        "a released claim retires its root");
}

/// A refusal in an older build's words goes on to the batch, which records it for
/// the disposition pass.
#[test]
fn an_older_builds_claim_refusal_is_still_recorded() {
    let (claimed, entry, _) = live_unfocused_root("root-settled-old-refusal");
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
        .root_read_errors.insert(entry.claim_id.clone(),
            json!({"code": "invalid_input", "message": ENGRAM_NAMED_ROOT_CLAIM_NOT_CURRENT}));
    claimed.state.recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    assert!(claimed.state.inner.lock().unwrap().engram_root_reclamation.refused_claims.iter()
        .any(|(store, claim)| store == &entry.store && claim == &entry.claim_id));
}

/// The recurring pass, driven directly on the one root it plans.
fn recurring_pass(claimed: &EOCheckedTurn, route: EngramRootReclamationRoute) {
    let plan = reclamation_plan(claimed);
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].route, route);
    claimed.state.run_engram_root_reclamation_pass(plan);
}

fn idle_route(claimed: &EOCheckedTurn) -> EngramRootReclamationRoute {
    EngramRootReclamationRoute::Current(claimed.session_id.clone())
}

/// The reason, at each authority boundary the pass reaches, that a turn opening
/// there would be unconfirmed. Boundaries are phases, not times.
fn watch_unconfirmed(
    claimed: &EOCheckedTurn,
    entry: &EngramWorkSourceRoot,
) -> Arc<Mutex<Vec<(String, EngramUnconfirmedRootReason)>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (recorded, inner, store, work) =
        (seen.clone(), claimed.state.inner.clone(), entry.store.clone(), entry.work_id.clone());
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = Some(Arc::new(move |at: &str| {
        let reason = engram_authority_work_unresolved_reason(&inner.lock().unwrap(), &store, &work);
        if let Some(reason) = reason {
            recorded.lock().unwrap().push((at.to_owned(), reason));
        }
    }));
    seen
}

fn assert_recurring_pass_left_it_alone(
    claimed: &EOCheckedTurn,
    entry: &EngramWorkSourceRoot,
    owner: &EngramAuthorityTransition,
) {
    assert_eq!(unresolved_reason(claimed, entry), None);
    assert_eq!(&authority_owner(claimed, entry), owner, "no owner was prepared for nothing");
}

/// The pass reads a live root and has nothing to settle on it, so a turn opening
/// at any point of the pass finds the root confirmed.
fn recurring_pass_leaves_a_live_root(
    claimed: &EOCheckedTurn,
    entry: &EngramWorkSourceRoot,
    owner: &EngramAuthorityTransition,
    route: EngramRootReclamationRoute,
) {
    let reads = root_reads(claimed);
    let seen = watch_unconfirmed(claimed, entry);
    recurring_pass(claimed, route);
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;
    assert!(root_reads(claimed) > reads, "the lifecycle read still happens");
    assert_eq!(*seen.lock().unwrap(), Vec::new(), "a turn opening inside the pass would be unconfirmed");
    assert_recurring_pass_left_it_alone(claimed, entry, owner);
}

#[test]
fn a_recurring_pass_leaves_an_idle_owners_live_root_confirmed() {
    let (claimed, entry, owner) = live_idle_root("recurring-idle");
    recurring_pass_leaves_a_live_root(&claimed, &entry, &owner, idle_route(&claimed));
    let capture = root_capture(&claimed);
    assert!(matches!(capture, Ok(EngramRootCapture::Recorded { .. })), "{capture:?}");
}

#[test]
fn a_recurring_pass_leaves_an_unfocused_live_root_confirmed() {
    let (claimed, entry, owner) = live_unfocused_root("recurring-unfocused");
    recurring_pass_leaves_a_live_root(&claimed, &entry, &owner, EngramRootReclamationRoute::Unfocused);
}

fn leave_root_reads_unanswered(claimed: &EOCheckedTurn, entry: &EngramWorkSourceRoot) {
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
        .root_read_errors.insert(entry.claim_id.clone(), json!({"code": "store_busy", "message": "try later"}));
}

/// An unanswered read learns nothing, and the idle owner's next turn opens confirmed.
#[test]
fn an_unanswered_recurring_read_leaves_an_idle_owners_live_root_confirmed() {
    let (claimed, entry, owner) = live_idle_root("recurring-idle-unanswered");
    leave_root_reads_unanswered(&claimed, &entry);
    recurring_pass(&claimed, idle_route(&claimed));
    let capture = root_capture(&claimed);
    assert!(matches!(capture, Ok(EngramRootCapture::Recorded { .. })), "{capture:?}");
    assert_recurring_pass_left_it_alone(&claimed, &entry, &owner);
}

#[test]
fn an_unanswered_recurring_read_leaves_an_unfocused_live_root_confirmed() {
    let (claimed, entry, owner) = live_unfocused_root("recurring-unfocused-unanswered");
    leave_root_reads_unanswered(&claimed, &entry);
    recurring_pass(&claimed, EngramRootReclamationRoute::Unfocused);
    assert_recurring_pass_left_it_alone(&claimed, &entry, &owner);
}

/// Nothing was left to publish, so a slow writer can't strand the root.
#[test]
fn a_recurring_pass_cannot_strand_an_idle_owners_live_root_when_a_publication_is_refused() {
    let (claimed, entry, owner) = live_idle_root("recurring-idle-refused");
    let refused = refuse_next_publication(&claimed, "before_publication");
    recurring_pass(&claimed, idle_route(&claimed));
    let capture = root_capture(&claimed);
    assert!(matches!(capture, Ok(EngramRootCapture::Recorded { .. })), "{capture:?}");
    assert!(!refused.load(std::sync::atomic::Ordering::SeqCst), "nothing was left to publish");
    assert_recurring_pass_left_it_alone(&claimed, &entry, &owner);
}

#[test]
fn a_recurring_pass_cannot_strand_an_unfocused_live_root_when_a_publication_is_refused() {
    let (claimed, entry, owner) = live_unfocused_root("recurring-unfocused-refused");
    let refused = refuse_next_publication(&claimed, "before_publication");
    recurring_pass(&claimed, EngramRootReclamationRoute::Unfocused);
    assert_eq!(unresolved_reason(&claimed, &entry), None);
    assert!(!refused.load(std::sync::atomic::Ordering::SeqCst), "nothing was left to publish");
    assert_recurring_pass_left_it_alone(&claimed, &entry, &owner);
}

/// A read that can't be served at all learns nothing either: the root stays, on
/// the same path as a busy store.
#[test]
fn an_unavailable_recurring_read_leaves_a_live_root_confirmed() {
    let (claimed, entry, owner) = live_idle_root("recurring-idle-unavailable");
    {
        let mut roots = claimed.transport.named_roots.lock().unwrap();
        let roots = roots.as_mut().unwrap();
        roots.root_reads.remove(&entry.claim_id);
        roots.read_bindings.remove(&entry.claim_id);
    }
    recurring_pass(&claimed, idle_route(&claimed));
    let capture = root_capture(&claimed);
    assert!(matches!(capture, Ok(EngramRootCapture::Recorded { .. })), "{capture:?}");
    assert_recurring_pass_left_it_alone(&claimed, &entry, &owner);
}

/// A live read with something to retire still goes on: the pass frees the slot.
#[test]
fn a_recurring_pass_still_retires_a_bound_root_whose_claim_was_released() {
    let (claimed, entry, _) = live_unfocused_root("recurring-released-claim");
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
        .root_reads.get_mut(&entry.claim_id).unwrap()["claim"] = json!({"state": "released"});
    recurring_pass(&claimed, EngramRootReclamationRoute::Unfocused);
    assert!(!claimed.state.inner.lock().unwrap().engram_work_source_roots.contains(&entry));
}

/// The read that found something to settle only decided: the owner reads for itself.
#[test]
fn a_recurring_pass_reads_a_root_it_retires_twice() {
    let (claimed, entry, _) = live_unfocused_root("recurring-two-reads");
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
        .root_reads.get_mut(&entry.claim_id).unwrap()["claim"] = json!({"state": "released"});
    let reads = root_reads(&claimed);
    recurring_pass(&claimed, EngramRootReclamationRoute::Unfocused);
    assert_eq!(root_reads(&claimed) - reads, 2);
    assert!(!claimed.state.inner.lock().unwrap().engram_work_source_roots.contains(&entry));
}

/// Above the floor a root can still lose its second read: the attempt fails and the
/// run is retained. There's no retry loop. At its next attempt the work is unsettled,
/// so it skips the deciding read and spends its one read with the full share.
#[test]
fn a_root_whose_owners_read_goes_unanswered_heals_on_its_next_attempt() {
    let (claimed, entry, _) = live_unfocused_root("recurring-owner-read-unanswered");
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
        .root_reads.get_mut(&entry.claim_id).unwrap()["claim"] = json!({"state": "released"});
    let (transport, claim) = (claimed.transport.clone(), entry.claim_id.clone());
    let once = std::sync::atomic::AtomicBool::new(false);
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = Some(Arc::new(move |at: &str| {
        // After the deciding read, while the owner is being prepared.
        if at == "before_delta" && !once.swap(true, std::sync::atomic::Ordering::SeqCst) {
            transport.named_roots.lock().unwrap().as_mut().unwrap().root_read_errors
                .insert(claim.clone(), json!({"code": "store_busy", "message": "try later"}));
        }
    }));
    recurring_pass(&claimed, EngramRootReclamationRoute::Unfocused);
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;
    assert_eq!(unresolved_reason(&claimed, &entry), Some(EngramUnconfirmedRootReason::WorkRunsUnresolved));
    assert_root_retained(&claimed, &entry);
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap().root_read_errors.remove(&entry.claim_id);
    claimed.state.engram_budget_clock().advance(ENGRAM_ROOT_RECLAMATION_COOLDOWN);
    let reads = root_reads(&claimed);
    recurring_pass(&claimed, EngramRootReclamationRoute::Unfocused);
    assert_eq!(root_reads(&claimed) - reads, 1, "an unsettled work has no deciding read");
    assert!(!claimed.state.inner.lock().unwrap().engram_work_source_roots.contains(&entry));
}

/// A skip adds no load: a live root costs one read, and its cooldown holds.
#[test]
fn a_live_root_is_read_once_per_cooldown() {
    let (claimed, entry, owner) = live_unfocused_root("recurring-cooldown");
    let reads = root_reads(&claimed);
    recurring_pass(&claimed, EngramRootReclamationRoute::Unfocused);
    assert_eq!(root_reads(&claimed) - reads, 1);
    assert!(reclamation_plan(&claimed).is_empty(), "it waits out its cooldown");
    claimed.state.engram_budget_clock().advance(ENGRAM_ROOT_RECLAMATION_COOLDOWN);
    recurring_pass(&claimed, EngramRootReclamationRoute::Unfocused);
    assert_eq!(root_reads(&claimed) - reads, 2);
    assert_eq!(authority_owner(&claimed, &entry), owner);
}

/// A read left unanswered is reported as the attempt's outcome, not as the work's.
#[test]
fn an_unanswered_recurring_read_is_reported_as_the_attempts_outcome() {
    let (claimed, entry, _) = live_unfocused_root("recurring-unanswered-outcome");
    leave_root_reads_unanswered(&claimed, &entry);
    recurring_pass(&claimed, EngramRootReclamationRoute::Unfocused);
    let now = claimed.state.engram_budget_clock().now();
    let status = engram_named_root_reclamation_text(&claimed.state.inner.lock().unwrap(), &entry, now);
    assert!(status.contains("try later"), "{status}");
    assert_eq!(claimed.state.inner.lock().unwrap().engram_work_naming_history[0].recovery_reason, None);
}

/// A naming that lands during the read is why the read learned something. It isn't
/// the pass's to supersede: no owner is prepared over it, and nothing is withheld.
#[test]
fn a_selection_changed_during_a_recurring_read_prepares_nothing() {
    let (claimed, entry, owner) = live_unfocused_root("recurring-read-race");
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
        .root_reads.get_mut(&entry.claim_id).unwrap()["claim"] = json!({"state": "released"});
    let plan = reclamation_plan(&claimed);
    let state = claimed.state.clone();
    TEST_ENGRAM_DURING_ROOT_LIFECYCLE_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            state.inner.lock().unwrap().engram_work_source_roots[0].generation += 1;
        }));
    });
    claimed.state.run_engram_root_reclamation_pass(plan);
    assert_eq!(unresolved_reason(&claimed, &entry), None);
    assert_eq!(authority_owner(&claimed, &entry), owner, "no owner was prepared over it");
    assert_eq!(claimed.state.inner.lock().unwrap().engram_work_source_roots.len(), 1);
}

/// With no proof of this run yet, the read has something to teach.
#[test]
fn a_recurring_pass_learns_a_run_it_has_no_proof_of() {
    let (claimed, entry, owner) = live_unfocused_root("recurring-unproven");
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let history = inner.engram_work_naming_history.iter_mut()
            .find(|history| history.store == entry.store && history.work_id == entry.work_id).unwrap();
        for proof in &mut history.proofs {
            proof.binding.run_id.push_str("-another");
        }
    }
    recurring_pass(&claimed, EngramRootReclamationRoute::Unfocused);
    let inner = claimed.state.inner.lock().unwrap();
    let history = inner.engram_work_naming_history.iter()
        .find(|history| history.store == entry.store && history.work_id == entry.work_id).unwrap();
    assert!(history.proofs.iter().any(|proof| engram_same_recovery_run(&proof.binding, &owner.binding)));
}

/// A work already unsettled isn't read past: the pass's owner settles it as before,
/// and a read left unanswered keeps evidence withheld.
#[test]
fn a_recurring_pass_keeps_withholding_a_work_that_is_unsettled() {
    let (claimed, entry, owner) = live_idle_root("recurring-unsettled");
    claimed.state.prepare_engram_authority(&entry.store, &owner.binding, Duration::from_secs(2)).unwrap();
    assert_eq!(root_capture(&claimed), Err(EngramUnconfirmedRootReason::WorkRunsUnresolved));
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
        .root_read_errors.insert(entry.claim_id.clone(), json!({"code": "store_busy", "message": "try later"}));
    recurring_pass(&claimed, idle_route(&claimed));
    assert_eq!(root_capture(&claimed), Err(EngramUnconfirmedRootReason::WorkRunsUnresolved));
}

#[test]
fn named_root_reclamation_completed_old_claim_frees_capacity() { recovery_frees_capacity("completed"); }
#[test]
fn named_root_reclamation_cancelled_old_claim_frees_capacity() { recovery_frees_capacity("cancelled"); }
#[test]
fn named_root_reclamation_released_old_claim_frees_capacity() { recovery_frees_capacity("released"); }
#[test]
fn named_root_reclamation_ended_old_binding_frees_capacity() { recovery_frees_capacity("ended"); }

fn own_clear_without_live_claim(proof_kind: &str) {
    let label = format!("root-reclamation-clear-{proof_kind}");
    let (claimed, entry) = settled_root(&label, proof_kind);
    let binds_before = claimed.transport.requests().iter()
        .filter(|request| request.request["operation"] == "named_root_bind").count();
    let result = claimed.state.name_engram_source_root(&claimed.session_id,
        EngramSourceRootRequest { work: entry.short_ref.clone(), path: None });
    assert!(result.is_ok(), "the original namer's ended entry clears without a held claim: {result:?}");
    assert!(engram_work_source_root_for_claim(
        &claimed.state.inner.lock().unwrap().engram_work_source_roots,
        &entry.store, &entry.work_id, &entry.claim_id).is_none());
    assert_eq!(claimed.transport.requests().iter()
        .filter(|request| request.request["operation"] == "named_root_bind").count(), binds_before,
        "host retirement cannot fabricate an Ended producer event for the old claim");
}

#[test]
fn named_root_reclamation_own_completed_clear_without_live_claim() { own_clear_without_live_claim("completed"); }
#[test]
fn named_root_reclamation_own_released_clear_without_live_claim() { own_clear_without_live_claim("released"); }

fn recovery_retains_uncertainty(proof_kind: &str) {
    let label = format!("root-reclamation-retain-{proof_kind}");
    let (claimed, entry) = settled_root(&label, proof_kind);
    claimed.state.recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    assert_eq!(engram_work_source_root_for_claim(
        &claimed.state.inner.lock().unwrap().engram_work_source_roots,
        &entry.store, &entry.work_id, &entry.claim_id), Some(&entry));
    let result = claimed.state.name_engram_source_root(&claimed.session_id,
        EngramSourceRootRequest { work: entry.short_ref.clone(), path: None });
    assert!(result.is_err(), "an open or unknown old binding cannot clear");
    assert_eq!(engram_work_source_root_for_claim(
        &claimed.state.inner.lock().unwrap().engram_work_source_roots,
        &entry.store, &entry.work_id, &entry.claim_id), Some(&entry));
}

#[test]
fn named_root_reclamation_live_old_binding_is_retained() { recovery_retains_uncertainty("live"); }
#[test]
fn named_root_reclamation_unknown_old_binding_is_retained() { recovery_retains_uncertainty("unknown"); }

fn reclamation_plan(claimed: &EOCheckedTurn) -> Vec<EngramRootReclamationTask> {
    let now = claimed.state.engram_budget_clock().now();
    let (snapshot, expected, disposition) = {
        let inner = claimed.state.inner.lock().unwrap();
        (EngramRootReclamationSnapshot::capture(&inner),
            engram_root_reclamation_plan_locked(&inner, now),
            engram_named_root_disposition_due_locked(&inner, now))
    };
    let plan = snapshot.plan(now);
    assert_eq!(plan.iter().map(|task| (&task.root, &task.route)).collect::<Vec<_>>(),
        expected.iter().map(|task| (&task.root, &task.route)).collect::<Vec<_>>(),
        "the snapshot plan must equal the original locked planner");
    assert_eq!(snapshot.disposition_due(now), disposition,
        "the snapshot disposition decision must equal the original locked scan");
    plan
}

fn assert_root_retained(claimed: &EOCheckedTurn, entry: &EngramWorkSourceRoot) {
    assert!(claimed.state.inner.lock().unwrap().engram_work_source_roots.contains(entry));
}

#[test]
fn named_root_reclamation_bounded_pass_retires_unfocused_without_provider_dispatch() {
    let (claimed, entry) = settled_root("reclamation-pass-unfocused", "completed");
    let plan = reclamation_plan(&claimed);
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].route, EngramRootReclamationRoute::Unfocused);
    claimed.state.run_engram_root_reclamation_pass(plan);
    let inner = claimed.state.inner.lock().unwrap();
    assert!(!inner.engram_work_source_roots.contains(&entry));
    assert_eq!(engram_named_root_capacity(&inner), 0);
    let notices = &inner.engram_work_naming_history[0].retirements;
    assert_eq!(notices.len(), 1);
    assert!(notices[0].published);
    assert!(!notices[0].delivered);
    assert!(matches!(claimed._runtime_rx.lock().unwrap().try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
}

#[test]
fn named_root_reclamation_idle_current_binding_uses_existing_recovery_owner() {
    let (claimed, entry) = settled_root("reclamation-idle-current", "completed");
    let binding = engram_root_journal(&claimed.state.inner.lock().unwrap().engram_named_root_journal,
        &entry.store, &entry.claim_id).unwrap().read_binding.clone().unwrap();
    claimed.record_mut(|record| {
        record.engram.work_binding = Some(binding.clone());
        record.session.status = SessionStatus::Idle;
    });
    claimed.transport.enable_named_roots(&claimed.session_id, &binding);
    let plan = reclamation_plan(&claimed);
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].route, EngramRootReclamationRoute::Current(claimed.session_id.clone()));
    claimed.state.run_engram_root_reclamation_pass(plan);
    assert!(!claimed.state.inner.lock().unwrap().engram_work_source_roots.contains(&entry));
    assert!(matches!(claimed._runtime_rx.lock().unwrap().try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
}

#[test]
fn named_root_reclamation_active_current_and_pending_intent_are_not_candidates() {
    let (claimed, entry) = settled_root("reclamation-excluded-owners", "completed");
    let binding = engram_root_journal(&claimed.state.inner.lock().unwrap().engram_named_root_journal,
        &entry.store, &entry.claim_id).unwrap().read_binding.clone().unwrap();
    claimed.record_mut(|record| {
        record.engram.work_binding = Some(binding);
        record.session.status = SessionStatus::Active;
    });
    assert!(reclamation_plan(&claimed).is_empty());
    claimed.record_mut(|record| record.session.status = SessionStatus::Idle);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let journal = &mut inner.engram_named_root_journal[0];
        journal.pending = Some(journal.confirmed.as_ref().unwrap().0.clone());
    }
    assert!(reclamation_plan(&claimed).is_empty());
    assert_root_retained(&claimed, &entry);
}

#[test]
fn named_root_reclamation_owner_becoming_current_during_read_retains_entry() {
    let (claimed, entry) = settled_root("reclamation-owner-race", "completed");
    let binding = engram_root_journal(&claimed.state.inner.lock().unwrap().engram_named_root_journal,
        &entry.store, &entry.claim_id).unwrap().read_binding.clone().unwrap();
    let plan = reclamation_plan(&claimed);
    let state = claimed.state.clone();
    let session_id = claimed.session_id.clone();
    during_owners_read(move || {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].engram.work_binding = Some(binding);
    });
    claimed.state.run_engram_root_reclamation_pass(plan);
    assert_root_retained(&claimed, &entry);
    assert!(claimed.state.inner.lock().unwrap().engram_work_naming_history[0].retirements.is_empty());
}

#[test]
fn named_root_reclamation_exact_selection_and_pending_journal_races_are_fenced() {
    for pending in [false, true] {
        let (claimed, entry) = settled_root("reclamation-selection-journal-race", "completed");
        let plan = reclamation_plan(&claimed);
        let state = claimed.state.clone();
        during_owners_read(move || {
            let mut inner = state.inner.lock().unwrap();
            if pending {
                let journal = &mut inner.engram_named_root_journal[0];
                journal.pending = Some(journal.confirmed.as_ref().unwrap().0.clone());
            } else {
                inner.engram_work_source_roots[0].generation += 1;
            }
        });
        claimed.state.run_engram_root_reclamation_pass(plan);
        let inner = claimed.state.inner.lock().unwrap();
        assert_eq!(inner.engram_work_source_roots.len(), 1);
        assert!(inner.engram_work_naming_history[0].retirements.is_empty());
        assert_eq!(inner.engram_work_source_roots[0].generation, entry.generation + u64::from(!pending));
        assert_eq!(inner.engram_named_root_journal[0].pending.is_some(), pending);
    }
}

#[test]
fn named_root_reclamation_planner_caps_reads_and_cooldown_advances_oldest_entries() {
    let (claimed, entry) = settled_root("reclamation-plan-cap", "unknown");
    let now = claimed.state.engram_budget_clock().now();
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let journal = inner.engram_named_root_journal[0].clone();
        for number in 1..17 {
            let mut root = entry.clone();
            root.work_id = format!("plan-work-{number}");
            root.claim_id = format!("plan-claim-{number}");
            root.named_at = format!("2026-10-02T00:00:{number:02}Z");
            let mut association = journal.clone();
            association.claim_id = root.claim_id.clone();
            let binding = association.read_binding.as_mut().unwrap();
            binding.work_id = root.work_id.clone();
            binding.claim_id = root.claim_id.clone();
            inner.engram_named_root_journal.push(association);
            inner.engram_work_source_roots.push(root);
        }
    }
    let first = reclamation_plan(&claimed);
    assert_eq!(first.len(), 8);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        for task in &first {
            inner.engram_root_reclamation.cooldowns.push((task.root.clone(), now + ENGRAM_ROOT_RECLAMATION_COOLDOWN));
        }
    }
    let next = reclamation_plan(&claimed);
    assert_eq!(next.len(), 8);
    assert!(next.iter().all(|next| first.iter().all(|old| old.root != next.root)));
    assert!(next.windows(2).all(|pair| pair[0].root.named_at <= pair[1].root.named_at));
}

#[test]
fn named_root_reclamation_deadline_stops_pass_without_retiring_unacknowledged_image() {
    let (claimed, entry) = settled_root("reclamation-pass-deadline", "completed");
    let plan = reclamation_plan(&claimed);
    let clock = claimed.state.engram_budget_clock();
    during_owners_read(move || clock.advance(ENGRAM_ROOT_RECLAMATION_BUDGET));
    claimed.state.run_engram_root_reclamation_pass(plan);
    let inner = claimed.state.inner.lock().unwrap();
    assert_eq!(engram_named_root_capacity(&inner), 1);
    assert!(inner.engram_work_naming_history[0].retirements.iter().all(|notice| !notice.published));
    assert!(inner.engram_work_source_roots.contains(&entry)
        || inner.engram_work_naming_history[0].retirements.iter().any(|notice| notice.selection == entry));
}
