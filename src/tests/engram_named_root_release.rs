// Owns the regressions for the operator view of the global named-root table
// and its audited release (src/engram_named_root_release.rs): every retained
// root names its naming session, claim binding, run association, reclamation
// status and last known run state, in the capacity refusal and the read-only
// view; a release frees exactly one stale root and never a live claim's.
// Does not own the automatic reclamation pass (engram_named_root_reclamation.rs
// in this directory, whose `settled_root` fixture this child module reuses).
// New module.
use super::*;

/// A retained root with no canonical run association: no journal exists for
/// its claim. `named_by` is its naming session.
fn orphan_of(entry: &EngramWorkSourceRoot, label: &str, named_by: &str, offset: u64) -> EngramWorkSourceRoot {
    let mut orphan = entry.clone();
    orphan.work_id = format!("orphan-work-{label}");
    orphan.claim_id = format!("orphan-claim-{label}");
    orphan.short_ref = format!("w-orphan-{label}");
    orphan.named_by_session = named_by.to_owned();
    orphan.generation = entry.generation + offset;
    orphan
}

fn retain(claimed: &ClaimedRoot, root: &EngramWorkSourceRoot) {
    claimed.state.inner.lock().unwrap().engram_work_source_roots.push(root.clone());
}

fn retained(claimed: &ClaimedRoot, root: &EngramWorkSourceRoot) -> bool {
    claimed.state.inner.lock().unwrap().engram_work_source_roots.contains(root)
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

/// Puts the claimed root's session into a quiet state: idle, no grant, no
/// checkpoint. `busy` sets one way it is not.
fn set_naming_session(claimed: &ClaimedRoot, busy: Option<&str>) {
    claimed.record(|record| {
        record.session.status = SessionStatus::Idle;
        record.engram.active_grant_id = None;
        record.engram.uncertain_grant_id = None;
        record.engram.checkpoint_in_progress = false;
        match busy {
            Some("turn") => record.session.status = SessionStatus::Active,
            Some("grant") => record.engram.active_grant_id = Some("grant-held".to_owned()),
            Some("checkpoint") => record.engram.checkpoint_in_progress = true,
            Some(other) => panic!("unknown busy state {other}"),
            None => {}
        }
    });
}

fn audit(claimed: &ClaimedRoot) -> Vec<EngramNamedRootReleaseAudit> {
    engram_named_root_release_audit(&claimed.state.inner.lock().unwrap())
}

fn row_for<'a>(rows: &'a [EngramNamedRootStatusRow], root: &EngramWorkSourceRoot) -> &'a EngramNamedRootStatusRow {
    rows.iter()
        .find(|row| row.work_id == root.work_id && row.claim_id == root.claim_id)
        .unwrap_or_else(|| panic!("no status row for {}: {rows:#?}", root.short_ref))
}

#[test]
fn each_retained_root_shows_its_naming_session_association_reclamation_and_lifecycle() {
    let (claimed, entry) = settled_root("release-status", "live");
    let orphan = orphan_of(&entry, "status", "session-gone", 100);
    retain(&claimed, &orphan);
    let now = claimed.state.engram_budget_clock().now();
    let rows = engram_named_root_status_locked(&claimed.state.inner.lock().unwrap(), now);
    assert_eq!(rows.len(), 2, "{rows:#?}");

    let settled = row_for(&rows, &entry);
    assert_eq!(settled.naming_session, claimed.session_id);
    assert!(settled.naming_session_state.starts_with("present"), "{settled:#?}");
    assert_eq!(settled.claim_binding, "none", "the session moved on to another claim");
    assert!(settled.association.contains("run binding"), "{settled:#?}");
    assert!(!settled.reclamation.is_empty(), "{settled:#?}");
    assert!(!settled.lifecycle.is_empty(), "{settled:#?}");

    let stale = row_for(&rows, &orphan);
    assert_eq!(stale.naming_session, "session-gone");
    assert_eq!(stale.naming_session_state, "gone");
    assert_eq!(stale.claim_binding, "none");
    assert!(stale.association.contains("no retained canonical run association"), "{stale:#?}");
    // No canonical reader is needed: the disposition pass releases a root
    // without an association whose naming session is gone.
    assert!(stale.reclamation.starts_with("disposition"), "{stale:#?}");
    assert!(stale.reclamation.contains("naming session is gone"), "{stale:#?}");
    assert_eq!(stale.lifecycle, "unknown (not read)");

    // The read-only view serves the same rows.
    assert_eq!(claimed.state.engram_named_root_status(), rows);
}

#[test]
fn the_capacity_refusal_lists_each_roots_status_row() {
    let (claimed, entry) = settled_root("release-capacity", "live");
    for number in 1..ENGRAM_WORK_SOURCE_ROOT_LIMIT {
        let orphan = orphan_of(&entry, &format!("capacity-{number}"), "session-gone", 100 + number as u64);
        retain(&claimed, &orphan);
    }
    assert_eq!(
        claimed.state.inner.lock().unwrap().engram_work_source_roots.len(),
        ENGRAM_WORK_SOURCE_ROOT_LIMIT
    );
    let refusal = claimed.state.engram_root_capacity_refusal();
    assert!(refusal.message.contains("naming session session-gone (gone)"), "{}", refusal.message);
    assert!(
        refusal.message.contains(&format!("naming session {} (present", claimed.session_id)),
        "{}",
        refusal.message
    );
    for field in ["claim binding: ", "association: ", "reclamation: ", "lifecycle: "] {
        assert!(refusal.message.contains(field), "{field}: {}", refusal.message);
    }
}

#[test]
fn a_release_never_touches_a_root_whose_claim_is_live() {
    let label = "release-live-claim";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let tree = add_claimed_root_worktree(&claimed.root);
    name_root(&claimed, label, Some(&tree), vec![claimed_root_held(label)]).unwrap();
    let entry = claimed.state.inner.lock().unwrap().engram_work_source_roots[0].clone();
    let error = claimed
        .state
        .release_engram_named_root(Some("session-operator"), release_request(&entry, "not needed"))
        .expect_err("a live claim's root is never released");
    assert!(error.message.contains("current binding"), "{}", error.message);
    assert!(retained(&claimed, &entry));
    assert!(audit(&claimed).is_empty());
}

#[test]
fn a_completed_items_root_is_released_through_its_canonical_proof_and_audited() {
    let (claimed, entry) = settled_root("release-completed", "completed");
    let response = claimed
        .state
        .release_engram_named_root(Some("session-operator"), release_request(&entry, "item completed"))
        .expect("a completed item's root is released");
    assert!(response.released);
    assert_eq!(response.route, "canonical");
    assert!(!retained(&claimed, &entry));
    let records = audit(&claimed);
    assert_eq!(records.len(), 1, "{records:#?}");
    assert_eq!(records[0].selection, entry);
    assert_eq!(records[0].route, "canonical");
    assert_eq!(records[0].reason, "item completed");
    assert_eq!(records[0].requested_by.as_deref(), Some("session-operator"));
}

#[test]
fn an_unassociated_root_whose_naming_session_is_gone_is_released_only_with_a_reason() {
    let (claimed, entry) = settled_root("release-orphan", "live");
    let orphan = orphan_of(&entry, "gone", "session-gone", 100);
    retain(&claimed, &orphan);

    let error = claimed
        .state
        .release_engram_named_root(Some("session-operator"), release_request(&orphan, "  "))
        .expect_err("a release needs a reason");
    assert!(error.message.contains("reason"), "{}", error.message);
    assert!(retained(&claimed, &orphan));

    let response = claimed
        .state
        .release_engram_named_root(
            Some("session-operator"),
            release_request(&orphan, "stale root of a finished lane"),
        )
        .expect("an unassociated root of a gone session is released on a reason");
    assert!(response.released);
    assert_eq!(response.route, "operator");
    assert!(!retained(&claimed, &orphan));
    assert!(retained(&claimed, &entry), "only the exact selection is released");
    let records = audit(&claimed);
    assert_eq!(records.len(), 1, "{records:#?}");
    assert_eq!(records[0].route, "operator");
    assert_eq!(records[0].reason, "stale root of a finished lane");
    assert_eq!(records[0].lifecycle, "unknown (not read)");
}

#[test]
fn a_release_the_writer_cannot_confirm_keeps_its_slot_reserved() {
    // Success needs the writer's commit of the removal and its audit. A
    // stopped writer acknowledges nothing: the caller is told, and the slot
    // the root held stays reserved by the unacknowledged record.
    let (claimed, entry) = settled_root("release-writer-stopped", "live");
    let orphan = orphan_of(&entry, "writer-stopped", "session-gone", 100);
    retain(&claimed, &orphan);
    let capacity_before = engram_named_root_capacity(&claimed.state.inner.lock().unwrap());
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(false));
    let result = claimed
        .state
        .release_engram_named_root(Some("session-operator"), release_request(&orphan, "stale"));
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(true));
    let error = result.expect_err("an unconfirmed release is not reported released");
    assert!(error.message.contains("slot stays reserved"), "{}", error.message);
    let inner = claimed.state.inner.lock().unwrap();
    assert!(!inner.engram_work_source_roots.contains(&orphan));
    assert_eq!(
        engram_named_root_capacity(&inner),
        capacity_before,
        "the unacknowledged release still reserves the slot"
    );
    let records = engram_named_root_release_audit(&inner);
    assert_eq!(records.len(), 1, "{records:#?}");
    assert!(!records[0].acknowledged);
    drop(inner);

    // Once the writer is back, the same request resumes that release: its
    // commit is confirmed and the slot freed, with no restart and no second
    // record.
    let response = claimed
        .state
        .release_engram_named_root(Some("session-operator"), release_request(&orphan, "stale"))
        .expect("the retry confirms the earlier release");
    assert!(response.released);
    assert_eq!(response.route, "operator");
    let inner = claimed.state.inner.lock().unwrap();
    assert_eq!(engram_named_root_capacity(&inner), capacity_before - 1);
    let records = engram_named_root_release_audit(&inner);
    assert_eq!(records.len(), 1, "{records:#?}");
    assert!(records[0].acknowledged);
}

#[test]
fn an_unassociated_root_of_a_naming_session_kept_aside_at_restore_is_refused() {
    let (claimed, entry) = settled_root("release-orphan-quarantined", "live");
    let orphan = orphan_of(&entry, "quarantined", "session-quarantined", 100);
    retain(&claimed, &orphan);
    claimed
        .state
        .inner
        .lock()
        .unwrap()
        .quarantined_persisted_session_ids
        .insert("session-quarantined".to_owned());
    let error = claimed
        .state
        .release_engram_named_root(Some("session-operator"), release_request(&orphan, "stale"))
        .expect_err("its session's binding and activity cannot be read");
    assert!(error.message.contains("kept aside at restore"), "{}", error.message);
    assert!(retained(&claimed, &orphan));
    assert!(audit(&claimed).is_empty());
}

#[test]
fn a_release_text_over_its_limit_or_with_control_characters_is_refused() {
    let (claimed, entry) = settled_root("release-text-bounds", "live");
    let orphan = orphan_of(&entry, "text", "session-gone", 100);
    retain(&claimed, &orphan);
    let long = "x".repeat(ENGRAM_NAMED_ROOT_RELEASE_TEXT_LIMIT + 1);
    for (reason, requested_by, why) in [
        (long.as_str(), None, "limited to"),
        ("stale\nengram> forged line", None, "control characters"),
        ("stale", Some("operator\u{7}"), "control characters"),
    ] {
        let error = claimed
            .state
            .release_engram_named_root(requested_by, release_request(&orphan, reason))
            .expect_err("the text is refused before anything is released");
        assert_eq!(error.status, StatusCode::BAD_REQUEST, "{}", error.message);
        assert!(error.message.contains(why), "{}", error.message);
    }
    assert!(retained(&claimed, &orphan));
    assert!(audit(&claimed).is_empty());
}

#[test]
fn a_confirmed_release_is_acknowledged_frees_its_slot_and_persists_its_audit() {
    let (claimed, entry) = settled_root("release-persisted", "live");
    let orphan = orphan_of(&entry, "persisted", "session-gone", 100);
    retain(&claimed, &orphan);
    let capacity_before = engram_named_root_capacity(&claimed.state.inner.lock().unwrap());
    claimed
        .state
        .release_engram_named_root(Some("session-operator"), release_request(&orphan, "stale"))
        .expect("the release is confirmed");
    let inner = claimed.state.inner.lock().unwrap();
    assert_eq!(engram_named_root_capacity(&inner), capacity_before - 1);
    let records = engram_named_root_release_audit(&inner);
    assert_eq!(records.len(), 1, "{records:#?}");
    assert!(records[0].acknowledged);
    // The record and the removal travel in the persisted metadata the
    // writer commits, which is what the release's fence reads back.
    let metadata: PersistedState = serde_json::from_value(
        serde_json::to_value(PersistedState::metadata_from_inner(&inner)).unwrap(),
    )
    .unwrap();
    assert!(engram_named_root_release_in_metadata(&records[0], &metadata));
}

#[test]
fn an_unassociated_root_of_a_quiet_naming_session_is_released() {
    // Neither the automatic pass nor an own-clear can read an unassociated
    // root, so a naming session that still exists cannot free it either. Once
    // that session holds no binding for the claim and is quiet, the operator
    // release may, on a reason.
    let (claimed, entry) = settled_root("release-orphan-present", "live");
    let orphan = orphan_of(&entry, "present", &claimed.session_id, 100);
    retain(&claimed, &orphan);
    set_naming_session(&claimed, None);
    let response = claimed
        .state
        .release_engram_named_root(Some("session-operator"), release_request(&orphan, "stale"))
        .expect("a quiet naming session's unassociated root is released");
    assert_eq!(response.route, "operator");
    assert!(!retained(&claimed, &orphan));
    assert_eq!(audit(&claimed).len(), 1);
}

#[test]
fn an_unassociated_root_of_a_busy_naming_session_is_refused() {
    for busy in ["turn", "grant", "checkpoint"] {
        let (claimed, entry) = settled_root(&format!("release-orphan-busy-{busy}"), "live");
        let orphan = orphan_of(&entry, busy, &claimed.session_id, 100);
        retain(&claimed, &orphan);
        set_naming_session(&claimed, Some(busy));
        let error = claimed
            .state
            .release_engram_named_root(Some("session-operator"), release_request(&orphan, "stale"))
            .expect_err("a busy naming session may yet act on the root");
        assert!(error.message.contains("is busy"), "{busy}: {}", error.message);
        assert!(
            !error.message.contains("termal_name_source_root"),
            "{busy}: the refusal claims no path that cannot free it: {}",
            error.message
        );
        assert!(retained(&claimed, &orphan), "{busy}");
        assert!(audit(&claimed).is_empty(), "{busy}");
    }
}

#[test]
fn an_unassociated_root_with_a_pending_naming_event_is_refused() {
    let (claimed, entry) = settled_root("release-orphan-pending", "live");
    let orphan = orphan_of(&entry, "pending", "session-gone", 100);
    retain(&claimed, &orphan);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let mut journal = engram_root_journal(
            &inner.engram_named_root_journal, &entry.store, &entry.claim_id,
        )
        .expect("the settled root has a claim journal")
        .clone();
        journal.claim_id = orphan.claim_id.clone();
        journal.read_binding = None;
        let mut event = journal.confirmed.as_ref().unwrap().0.clone();
        event.root = orphan.clone();
        journal.pending = Some(event);
        inner.engram_named_root_journal.push(journal);
    }
    let error = claimed
        .state
        .release_engram_named_root(Some("session-operator"), release_request(&orphan, "stale"))
        .expect_err("a pending naming event belongs to its owner");
    assert!(error.message.contains("pending"), "{}", error.message);
    assert!(retained(&claimed, &orphan));
    assert!(audit(&claimed).is_empty());
}

#[test]
fn a_root_its_canonical_authority_keeps_is_not_released() {
    // An associated root goes only through the canonical proof: a run the
    // authority still reads as live keeps its root, with no audit row.
    let (claimed, entry) = settled_root("release-canonical-keeps", "live");
    claimed
        .state
        .release_engram_named_root(Some("session-operator"), release_request(&entry, "stale"))
        .expect_err("a live run keeps its root");
    assert!(retained(&claimed, &entry));
    assert!(audit(&claimed).is_empty());
}

#[test]
fn the_release_audit_keeps_only_its_newest_records() {
    let (claimed, entry) = settled_root("release-audit-cap", "live");
    let mut inner = claimed.state.inner.lock().unwrap();
    let mut first = None;
    for number in 0..=ENGRAM_NAMED_ROOT_RELEASE_AUDIT_LIMIT {
        let mut record = engram_named_root_release_record(
            &entry,
            EngramNamedRootReleaseRoute::Operator,
            &format!("reason {number}"),
            None,
            "unknown (not read)",
        );
        record.acknowledged = true;
        first.get_or_insert_with(|| record.id.clone());
        engram_named_root_record_release_locked(&mut inner, record);
    }
    let records = engram_named_root_release_audit(&inner);
    assert_eq!(records.len(), ENGRAM_NAMED_ROOT_RELEASE_AUDIT_LIMIT);
    assert!(records.iter().all(|record| Some(&record.id) != first.as_ref()), "the oldest went");
    assert_eq!(
        records.last().unwrap().reason,
        format!("reason {}", ENGRAM_NAMED_ROOT_RELEASE_AUDIT_LIMIT)
    );
}

#[test]
fn a_blank_requested_by_is_recorded_as_absent_and_a_given_one_trimmed() {
    let (claimed, entry) = settled_root("release-requested-by", "live");
    let blank = orphan_of(&entry, "blank-by", "session-gone", 100);
    let named = orphan_of(&entry, "named-by", "session-gone", 101);
    retain(&claimed, &blank);
    retain(&claimed, &named);
    claimed
        .state
        .release_engram_named_root(Some("   "), release_request(&blank, "stale"))
        .expect("released");
    claimed
        .state
        .release_engram_named_root(Some("  Termal::Fable2 "), release_request(&named, "stale"))
        .expect("released");
    let records = audit(&claimed);
    assert_eq!(records[0].requested_by, None);
    assert_eq!(records[1].requested_by.as_deref(), Some("Termal::Fable2"));
}

#[test]
fn an_unconfirmed_release_survives_the_audit_rollover() {
    // An unacknowledged record is a pending obligation (its retry, and the
    // operator route's reserved slot): trimming completed history never
    // drops it, however many releases follow.
    let (claimed, entry) = settled_root("release-audit-pending", "live");
    let mut inner = claimed.state.inner.lock().unwrap();
    let pending = engram_named_root_release_record(
        &entry,
        EngramNamedRootReleaseRoute::Operator,
        "pending",
        None,
        "unknown (not read)",
    );
    engram_named_root_record_release_locked(&mut inner, pending.clone());
    for number in 0..=ENGRAM_NAMED_ROOT_RELEASE_AUDIT_LIMIT {
        let mut record = engram_named_root_release_record(
            &entry,
            EngramNamedRootReleaseRoute::Operator,
            &format!("reason {number}"),
            None,
            "unknown (not read)",
        );
        record.acknowledged = true;
        engram_named_root_record_release_locked(&mut inner, record);
    }
    let records = engram_named_root_release_audit(&inner);
    assert_eq!(records.len(), ENGRAM_NAMED_ROOT_RELEASE_AUDIT_LIMIT);
    assert!(
        records.iter().any(|record| record.id == pending.id && !record.acknowledged),
        "the pending release is kept"
    );
}

#[test]
fn a_canonical_release_whose_authority_commit_fails_keeps_its_intent_for_the_retry() {
    // The operator's intent is recorded before the canonical retirement. When
    // the retirement fails before removing the root, the intent is withdrawn.
    // When it removes the root but its publication fails, the unacknowledged
    // record stays, and the identical request resumes it.
    //
    // Before the removal: a stopped writer fails the authority's prepare
    // step, so nothing was retired and the intent is withdrawn.
    let (claimed, entry) = settled_root("release-canonical-prepare-fails", "completed");
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(false));
    let result = claimed
        .state
        .release_engram_named_root(Some("session-operator"), release_request(&entry, "item completed"));
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(true));
    result.expect_err("a retirement that could not start is not reported released");
    assert!(retained(&claimed, &entry), "nothing was retired");
    assert!(audit(&claimed).is_empty(), "the intent is withdrawn");

    // After the removal: the candidate image (removal and intent) committed,
    // then the publication ran out of the release's own authority budget.
    let (claimed, entry) = settled_root("release-canonical-authority-fails", "completed");
    let clock = claimed.state.engram_budget_clock();
    let expired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hook_expired = expired.clone();
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary =
        Some(Arc::new(move |phase| {
            if phase == "before_publication"
                && !hook_expired.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                clock.advance(ENGRAM_NAMED_ROOT_RELEASE_AUTHORITY_BUDGET + Duration::from_secs(1));
            }
        }));
    let result = claimed
        .state
        .release_engram_named_root(Some("session-operator"), release_request(&entry, "item completed"));
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;
    assert!(expired.load(std::sync::atomic::Ordering::SeqCst), "the publication was reached");
    result.expect_err("an unpublished retirement is not reported released");
    assert!(!retained(&claimed, &entry), "the retirement removed the root");
    let records = audit(&claimed);
    assert_eq!(records.len(), 1, "the operator's intent is kept: {records:#?}");
    assert_eq!(records[0].route, "canonical");
    assert_eq!(records[0].reason, "item completed");
    assert!(!records[0].acknowledged);

    // A restart here keeps the canonical record pending, since its retirement
    // is still unpublished, so the identical retry can still resume it. A
    // removed operator release is committed, so it comes back done; an intent
    // whose selection is still retained never happened, so it is dropped.
    {
        let mut persisted = PersistedState::from_inner(&claimed.state.inner.lock().unwrap());
        let pending = persisted.engram_named_root_release_audit[0].clone();
        let mut operator = pending.clone();
        operator.id = "restored-operator".to_owned();
        operator.route = "operator".to_owned();
        operator.selection.generation += 1000;
        let mut retained_intent = pending.clone();
        retained_intent.id = "restored-retained".to_owned();
        retained_intent.selection.generation += 2000;
        // The same selection, credited to a transition that did not retire it.
        let mut foreign = pending.clone();
        foreign.id = "restored-foreign".to_owned();
        foreign.authority_owner = Some("another-authority-owner".to_owned());
        assert!(pending.authority_owner.is_some(), "the release recorded its own transition");
        persisted
            .engram_work_source_roots
            .push(retained_intent.selection.clone());
        persisted
            .engram_named_root_release_audit
            .extend([operator, retained_intent, foreign]);
        let restored = persisted.into_inner().expect("the persisted state restores");
        let restored_record = |id: &str| {
            restored
                .engram_named_root_release_audit
                .iter()
                .find(|record| record.id == id)
                .cloned()
        };
        assert!(
            !restored_record(&pending.id).expect("the canonical record is kept").acknowledged,
            "an unpublished canonical release stays pending across a restart"
        );
        assert!(
            restored_record("restored-operator").expect("the operator record is kept").acknowledged,
            "a committed operator release comes back done"
        );
        assert!(
            restored_record("restored-retained").is_none(),
            "an intent whose selection is still retained is dropped"
        );
        assert!(
            restored_record("restored-foreign").is_none(),
            "a canonical record another owner's retirement does not name is dropped"
        );
        assert_eq!(engram_named_root_unacknowledged_releases(&restored), 0);
    }

    let unpublished = |claimed: &ClaimedRoot| {
        let inner = claimed.state.inner.lock().unwrap();
        let unpublished = inner
            .engram_work_naming_history
            .iter()
            .flat_map(|history| &history.retirements)
            .any(|notice| notice.selection == entry && !notice.published);
        (unpublished, engram_named_root_capacity(&inner))
    };
    let (pending, reserved) = unpublished(&claimed);
    assert!(pending, "the retirement is committed but not published");
    take_engram_root_reclamation_emitted();

    let response = claimed
        .state
        .release_engram_named_root(Some("session-operator"), release_request(&entry, "item completed"))
        .expect("the retry resumes the recorded intent");
    assert_eq!(response.route, "canonical");
    let records = audit(&claimed);
    assert_eq!(records.len(), 1, "{records:#?}");
    assert!(records[0].acknowledged);
    let (pending, capacity) = unpublished(&claimed);
    assert!(!pending, "the retry published the retirement");
    assert_eq!(capacity + 1, reserved, "the retirement's slot is free");
    let emitted = take_engram_root_reclamation_emitted();
    assert!(
        emitted.iter().any(|line| line.starts_with("engram> named-root release route=canonical")
            && line.contains("reason=\"item completed\"")),
        "the resumed release writes its audit line: {emitted:#?}"
    );
}

#[test]
fn a_canonical_release_does_not_credit_a_retirement_another_owner_made() {
    // Another owner (here an unfocused reclamation run, as the automatic pass
    // makes) retires the same selection while this release's own authority
    // owner is being confirmed. This release removed nothing, so its intent
    // is withdrawn and no audit credits the operator with that retirement.
    let (claimed, entry) = settled_root("release-canonical-raced", "completed");
    let other = claimed.state.clone();
    let raced = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hook_raced = raced.clone();
    let hook_entry = entry.clone();
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary =
        Some(Arc::new(move |phase| {
            if phase == "before_owner_check"
                && !hook_raced.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                let deadline = other.engram_budget_clock().now() + ENGRAM_ROOT_RECLAMATION_BUDGET;
                assert!(
                    other
                        .recover_one_engram_root_until(
                            &hook_entry,
                            &EngramRootReclamationRoute::Unfocused,
                            deadline,
                        )
                        .expect("the other owner retires the root"),
                    "the other owner retired and published it"
                );
            }
        }));
    let result = claimed
        .state
        .release_engram_named_root(Some("session-operator"), release_request(&entry, "item completed"));
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;
    assert!(raced.load(std::sync::atomic::Ordering::SeqCst), "the other owner ran");
    let error = result.expect_err("a retirement this release did not make is not its release");
    assert!(error.message.contains("another owner"), "{}", error.message);
    assert!(!retained(&claimed, &entry), "the other owner's retirement stands");
    assert!(audit(&claimed).is_empty(), "no audit credits the operator: {:#?}", audit(&claimed));

    // The identical request afterwards finds nothing of its own to resume.
    let error = claimed
        .state
        .release_engram_named_root(Some("session-operator"), release_request(&entry, "item completed"))
        .expect_err("nothing to resume");
    assert!(error.message.contains("no longer retained"), "{}", error.message);
    assert!(audit(&claimed).is_empty());
}

#[test]
fn a_canonical_release_whose_audit_is_unconfirmed_resumes_on_retry() {
    // The canonical retirement is acknowledged on the authority's own fence;
    // the operator's audit of it is confirmed separately. If that fails, the
    // identical request resumes the audit's confirmation.
    let (claimed, entry) = settled_root("release-canonical-retry", "completed");
    TEST_ENGRAM_NAMED_ROOT_RELEASE_UNCONFIRMED.with(|unconfirmed| unconfirmed.set(true));
    let result = claimed
        .state
        .release_engram_named_root(Some("session-operator"), release_request(&entry, "item completed"));
    TEST_ENGRAM_NAMED_ROOT_RELEASE_UNCONFIRMED.with(|unconfirmed| unconfirmed.set(false));
    let error = result.expect_err("an unconfirmed audit is not reported released");
    assert!(error.message.contains("unconfirmed"), "{}", error.message);
    assert!(!retained(&claimed, &entry), "the canonical retirement itself happened");
    let records = audit(&claimed);
    assert_eq!(records.len(), 1, "{records:#?}");
    assert_eq!(records[0].route, "canonical");
    assert!(!records[0].acknowledged);

    let response = claimed
        .state
        .release_engram_named_root(Some("session-operator"), release_request(&entry, "item completed"))
        .expect("the retry confirms the audit");
    assert_eq!(response.route, "canonical");
    let records = audit(&claimed);
    assert_eq!(records.len(), 1, "{records:#?}");
    assert!(records[0].acknowledged);
}

#[test]
fn a_release_of_a_selection_that_changed_is_refused() {
    let (claimed, entry) = settled_root("release-changed", "live");
    let orphan = orphan_of(&entry, "changed", "session-gone", 100);
    retain(&claimed, &orphan);
    let mut request = release_request(&orphan, "stale");
    request.generation += 1;
    let error = claimed
        .state
        .release_engram_named_root(Some("session-operator"), request)
        .expect_err("only the exact retained selection is released");
    assert!(error.message.contains("selection"), "{}", error.message);
    assert!(retained(&claimed, &orphan));
}
