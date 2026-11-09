// A pending name whose claim was recovered under a newer fence. Engram refuses a
// Bound under any fence but the claim's current one, so that name's exact retry
// can never apply, and naming must not stay blocked behind it.
// Owns: that sequence, and what must still hold for a name with no such proof.
// Does not own: the rest of naming's refusal and retry behaviour, which stays in
// engram_source_root_naming.rs and engram_source_root_off_rebind.rs.
use super::*;

fn roots<R>(claimed: &EOCheckedTurn, change: impl FnOnce(&mut TestEngramNamedRoots) -> R) -> R {
    change(
        claimed
            .transport
            .named_roots
            .lock()
            .unwrap()
            .as_mut()
            .unwrap(),
    )
}

struct StaleFence {
    claimed: EOCheckedTurn,
    label: &'static str,
    first: PathBuf,
    other: PathBuf,
}

impl StaleFence {
    /// A claim at its first fence with two worktrees to name, and Engram
    /// checking the fence of every Bound.
    fn new(label: &'static str) -> Self {
        let claimed = EOCheckedTurn::idle(label, true);
        let first = naming_worktree(&claimed.root);
        let other = claimed.root.join(".worktrees").join("other");
        run_git_test_command(
            &claimed.root,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "other",
                other.to_str().unwrap(),
            ],
        );
        let binding = naming_claim(label).items[0]
            .control_binding
            .clone()
            .unwrap();
        roots(&claimed, |model| {
            model
                .claim_fences
                .insert(binding.claim_id.clone(), binding.claim_fence);
        });
        Self {
            claimed,
            label,
            first,
            other,
        }
    }

    /// The first send refused, as Engram refuses one for a lapsed claim, so its
    /// intent is pending under the first fence.
    fn refused_while_lapsed(label: &'static str) -> Self {
        let sequence = Self::new(label);
        roots(&sequence.claimed, |model| model.refuse_next = true);
        let refused = sequence.name(&sequence.first, 10).unwrap_err();
        assert!(
            refused.message.contains("named_root_binding_refused"),
            "{refused:?}"
        );
        assert_eq!(sequence.pending_fence(), Some(10));
        sequence
    }

    /// The holder claims the lapsed work again: the same claim and run, a newer fence.
    fn recover(&self) {
        let claim = naming_claim(self.label).items[0].claim_id.clone();
        roots(&self.claimed, |model| {
            model.claim_fences.insert(claim, 11);
        });
    }

    fn name(&self, path: &FsPath, fence: i64) -> Result<EngramSourceRootResponse, ApiError> {
        let mut held = naming_claim(self.label);
        held.items[0].claim_fence = fence;
        held.items[0].control_binding.as_mut().unwrap().claim_fence = fence;
        request_naming(&self.claimed, self.label, Some(path), vec![held])
    }

    fn clear(&self, fence: i64) -> Result<EngramSourceRootResponse, ApiError> {
        let mut held = naming_claim(self.label);
        held.items[0].claim_fence = fence;
        held.items[0].control_binding.as_mut().unwrap().claim_fence = fence;
        request_naming(&self.claimed, self.label, None, vec![held])
    }

    fn pending_fence(&self) -> Option<i64> {
        self.claimed
            .state
            .inner
            .lock()
            .unwrap()
            .engram_named_root_journal
            .iter()
            .find_map(|journal| journal.pending.as_ref())
            .map(|event| event.root.claim_fence)
    }

    fn sent_fences(&self) -> Vec<i64> {
        self.claimed
            .transport
            .requests()
            .into_iter()
            .filter(|request| request.request["operation"] == "named_root_bind")
            .map(|request| request.request["claim_fence"].as_i64().unwrap())
            .collect()
    }

    fn assert_named(&self, named: &EngramSourceRootResponse, path: &FsPath, fence: i64) {
        assert_eq!(self.pending_fence(), None, "nothing is left pending");
        let inner = self.claimed.state.inner.lock().unwrap();
        let root = inner
            .engram_work_source_roots
            .iter()
            .find(|root| root.generation == named.generation)
            .expect("the named root is published");
        assert_eq!(root.claim_fence, fence);
        assert_eq!(
            engram_canonical_path_key(FsPath::new(&root.root)),
            engram_canonical_path_key(path),
        );
    }
}

/// The reported sequence: another root is told to wait, the exact retry is refused
/// as stale, and after that any naming has to be able to finish.
#[test]
fn a_stale_pending_name_does_not_block_another_root() {
    let sequence = StaleFence::refused_while_lapsed("stale-fence-other-root");
    sequence.recover();
    let blocked = sequence.name(&sequence.other, 11).unwrap_err();
    assert!(blocked.message.contains("remains pending"), "{blocked:?}");
    assert_eq!(
        sequence.pending_fence(),
        Some(10),
        "not overtaken before its retry"
    );
    let retried = sequence.name(&sequence.first, 11).unwrap_err();
    assert!(retried.message.contains("is stale"), "{retried:?}");
    let named = sequence
        .name(&sequence.other, 11)
        .unwrap_or_else(|error| panic!("stuck behind the stale name: {error:?}"));
    sequence.assert_named(&named, &sequence.other, 11);
    assert_eq!(sequence.sent_fences(), [10, 10, 11]);
    assert!(retried.message.contains("name a root again"), "{retried:?}");
}

#[test]
fn a_stale_pending_name_does_not_block_its_own_root_named_again() {
    let sequence = StaleFence::refused_while_lapsed("stale-fence-same-root");
    sequence.recover();
    let retried = sequence.name(&sequence.first, 11).unwrap_err();
    assert!(retried.message.contains("is stale"), "{retried:?}");
    let named = sequence
        .name(&sequence.first, 11)
        .unwrap_or_else(|error| panic!("stuck behind the stale name: {error:?}"));
    sequence.assert_named(&named, &sequence.first, 11);
    assert_eq!(sequence.sent_fences(), [10, 10, 11]);
}

/// With the work revised after the recovery, Engram has no record of that revision
/// at the old fence and answers with a mismatch instead: the same dead end.
#[test]
fn a_stale_pending_name_refused_as_a_mismatch_does_not_block_naming() {
    let sequence = StaleFence::refused_while_lapsed("stale-fence-mismatch");
    sequence.recover();
    roots(&sequence.claimed, |model| model.revision_moved = true);
    let retried = sequence.name(&sequence.first, 11).unwrap_err();
    assert!(
        retried.message.contains("work_claim_mismatch"),
        "{retried:?}"
    );
    assert!(retried.message.contains("name a root again"), "{retried:?}");
    let named = sequence
        .name(&sequence.other, 11)
        .unwrap_or_else(|error| panic!("stuck behind the stale name: {error:?}"));
    sequence.assert_named(&named, &sequence.other, 11);
}

/// A stale name whose worktree is gone settles the same way, and another root can be named.
#[test]
fn a_stale_pending_name_whose_worktree_is_gone_does_not_block_naming() {
    let sequence = StaleFence::refused_while_lapsed("stale-fence-worktree-gone");
    sequence.recover();
    run_git_test_command(
        &sequence.claimed.root,
        &[
            "worktree",
            "remove",
            "--force",
            sequence.first.to_str().unwrap(),
        ],
    );
    let retried = sequence.name(&sequence.first, 11).unwrap_err();
    assert!(retried.message.contains("is stale"), "{retried:?}");
    assert!(retried.message.contains("name a root again"), "{retried:?}");
    assert_eq!(sequence.pending_fence(), None);
    let named = sequence
        .name(&sequence.other, 11)
        .unwrap_or_else(|error| panic!("stuck behind the stale name: {error:?}"));
    sequence.assert_named(&named, &sequence.other, 11);
}

/// A mismatch alone proves nothing: with no newer fence shown, it stays pending.
#[test]
fn a_mismatch_with_no_fence_shown_stays_pending() {
    let sequence = StaleFence::refused_while_lapsed("stale-fence-mismatch-no-fence");
    roots(&sequence.claimed, |model| {
        model.revision_moved = true;
        model.read_omits_claim = true;
    });
    sequence.recover();
    let retried = sequence.name(&sequence.first, 11).unwrap_err();
    assert!(
        retried.message.contains("work_claim_mismatch"),
        "{retried:?}"
    );
    assert!(
        retried.message.contains("retry this same request"),
        "{retried:?}"
    );
    assert_eq!(sequence.pending_fence(), Some(10));
}

/// A name whose reporter is gone, once the read shows its fence superseded, is retired
/// by that read, and a fresh name goes through.
#[test]
fn an_orphaned_name_under_a_superseded_fence_is_retired_by_the_read() {
    let mut sequence = StaleFence::refused_while_lapsed("stale-fence-orphan");
    sequence.recover();
    let label = sequence.label;
    replace_or_unbind_naming_session(&mut sequence.claimed, label, true);
    let binding = sequence
        .claimed
        .record_mut(|record| record.engram.work_binding.clone());
    sequence
        .claimed
        .canonical_root_readback(
            &sequence.claimed.session_id,
            binding.as_ref(),
            None,
            u64::MAX,
        )
        .unwrap();
    assert_eq!(sequence.pending_fence(), None);
    assert!(
        sequence
            .claimed
            .state
            .inner
            .lock()
            .unwrap()
            .engram_named_root_journal
            .iter()
            .any(|journal| journal
                .reconciliation
                .as_ref()
                .is_some_and(|read| read.settled))
    );
    let named = sequence
        .name(&sequence.other, 11)
        .unwrap_or_else(|error| panic!("stuck behind the retired name: {error:?}"));
    sequence.assert_named(&named, &sequence.other, 11);
}

/// The caller may not have made the first request: the refusal names what to retry.
#[test]
fn a_blocked_naming_says_which_root_is_pending() {
    let sequence = StaleFence::refused_while_lapsed("stale-fence-text");
    let blocked = sequence.name(&sequence.other, 10).unwrap_err();
    let pending = naming_pending_root(&sequence.claimed);
    assert!(
        blocked.message.contains(&pending),
        "{pending} not in {blocked:?}"
    );
}

/// With nothing settled, the refusal says so: the same request stays the way on.
#[test]
fn an_unsettled_refusal_says_the_same_request_is_still_owed() {
    let sequence = StaleFence::refused_while_lapsed("stale-fence-unsettled-text");
    roots(&sequence.claimed, |model| model.refuse_next = true);
    let retried = sequence.name(&sequence.first, 10).unwrap_err();
    assert!(
        retried.message.contains("retry this same request"),
        "{retried:?}"
    );
    assert!(
        !retried.message.contains("name a root again"),
        "{retried:?}"
    );
}

/// A clear has no fence for a read to supersede, so a refused one can stay pending
/// with nothing to prove it settled. The refusal gives the way out, and that way works.
#[test]
fn a_refused_clear_says_what_to_do_and_a_release_settles_it() {
    let sequence = StaleFence::new("stale-fence-refused-clear");
    let named = sequence.name(&sequence.first, 10).unwrap();
    roots(&sequence.claimed, |model| model.refuse_next = true);
    let refused = sequence.clear(10).unwrap_err();
    assert!(
        refused.message.contains("retry this same clear"),
        "{refused:?}"
    );
    assert!(
        refused
            .message
            .contains("release the claim and claim it again"),
        "{refused:?}"
    );
    assert_eq!(sequence.pending_fence(), Some(10));

    let blocked = sequence.name(&sequence.other, 10).unwrap_err();
    assert!(
        blocked.message.contains("clear of the named root"),
        "{blocked:?}"
    );
    assert!(
        blocked.message.contains("retry that clear first"),
        "{blocked:?}"
    );
    assert_eq!(sequence.pending_fence(), Some(10), "not overtaken");

    // Released and claimed again: Engram raises the fence on each, and the release
    // stays the canonical state until a later name. The clear still can't apply.
    let claim = naming_claim(sequence.label).items[0].claim_id.clone();
    roots(&sequence.claimed, |model| {
        model
            .released
            .insert(claim.clone(), named.generation as i64);
        model.claim_fences.insert(claim, 12);
        model.refuse_next = true;
    });
    let settled = sequence.clear(12).unwrap_err();
    assert!(
        settled.message.contains("no longer blocks naming"),
        "{settled:?}"
    );
    assert!(
        !settled.message.contains("can no longer apply"),
        "{settled:?}"
    );
    assert_eq!(sequence.pending_fence(), None);
    let again = sequence
        .name(&sequence.other, 12)
        .unwrap_or_else(|error| panic!("stuck behind the settled clear: {error:?}"));
    sequence.assert_named(&again, &sequence.other, 12);
}

/// An end the host raised isn't the caller's to retry, and nothing else sends it yet:
/// the refusal says so rather than promising a retry or asking for a clear.
#[test]
fn a_blocked_naming_behind_a_host_raised_end_says_to_report_it() {
    let sequence = StaleFence::new("stale-fence-host-raised-end");
    sequence.name(&sequence.first, 10).unwrap();
    roots(&sequence.claimed, |model| model.refuse_next = true);
    sequence.clear(10).unwrap_err();
    sequence
        .claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_named_root_journal
        .iter_mut()
        .find_map(|journal| journal.pending.as_mut())
        .unwrap()
        .end_reason = Some(EngramNamedRootEndReason::RootInvalid);
    let blocked = sequence.name(&sequence.other, 10).unwrap_err();
    assert!(
        blocked
            .message
            .contains("report this to the TermAl coordinator"),
        "{blocked:?}"
    );
    assert!(!blocked.message.contains("retry that clear"), "{blocked:?}");
}

/// A refusal alone proves nothing: under the current fence the name may still apply.
#[test]
fn a_refused_retry_under_the_current_fence_still_cannot_be_overtaken() {
    let sequence = StaleFence::refused_while_lapsed("stale-fence-current");
    roots(&sequence.claimed, |model| model.refuse_next = true);
    let retried = sequence.name(&sequence.first, 10).unwrap_err();
    assert!(
        retried.message.contains("named_root_binding_refused"),
        "{retried:?}"
    );
    assert_eq!(sequence.pending_fence(), Some(10));
    let blocked = sequence.name(&sequence.other, 10).unwrap_err();
    assert!(blocked.message.contains("remains pending"), "{blocked:?}");
    assert_eq!(sequence.pending_fence(), Some(10));
    assert_eq!(sequence.sent_fences(), [10, 10]);
}

/// A send whose reply was lost did apply: a newer fence doesn't discard it, and its
/// retry confirms the original event.
#[test]
fn a_lost_reply_under_an_old_fence_still_confirms_its_own_event() {
    let sequence = StaleFence::new("stale-fence-lost-reply");
    roots(&sequence.claimed, |model| model.lose_next_reply = true);
    let lost = sequence.name(&sequence.first, 10).unwrap_err();
    assert!(
        !lost.message.contains("named_root_binding_refused"),
        "{lost:?}"
    );
    let generation = naming_pending_generation(&sequence.claimed);
    sequence.recover();
    let named = sequence.name(&sequence.first, 11).unwrap();
    assert_eq!(
        named.generation, generation,
        "the original event, not a new one"
    );
    sequence.assert_named(&named, &sequence.first, 10);
    assert_eq!(sequence.sent_fences(), [10, 10]);
}

/// A read that doesn't report the fence, as from an older Engram, proves nothing.
#[test]
fn a_stale_pending_name_read_without_a_fence_stays_pending() {
    let sequence = StaleFence::refused_while_lapsed("stale-fence-no-claim-block");
    sequence.recover();
    roots(&sequence.claimed, |model| model.read_omits_claim = true);
    let retried = sequence.name(&sequence.first, 11).unwrap_err();
    assert!(retried.message.contains("is stale"), "{retried:?}");
    assert_eq!(sequence.pending_fence(), Some(10));
    let blocked = sequence.name(&sequence.other, 11).unwrap_err();
    assert!(blocked.message.contains("remains pending"), "{blocked:?}");
}

/// Unreadable after the refusal: nothing is known, so the name stays pending.
#[test]
fn a_stale_pending_name_with_an_unanswered_read_stays_pending() {
    let sequence = StaleFence::refused_while_lapsed("stale-fence-unanswered-read");
    sequence.recover();
    let claim = naming_claim(sequence.label).items[0].claim_id.clone();
    roots(&sequence.claimed, |model| {
        model.fail_reads_after_stale_refusal = Some(claim)
    });
    let retried = sequence.name(&sequence.first, 11).unwrap_err();
    assert!(
        !retried.message.contains("name a root again"),
        "{retried:?}"
    );
    assert_eq!(sequence.sent_fences(), [10, 10], "it did reach the refusal");
    assert_eq!(sequence.pending_fence(), Some(10));
}

/// What a read settles, without a refusal: only a Bound, and only a shown newer fence.
#[test]
fn only_a_bound_is_settled_by_a_newer_fence() {
    let sequence = StaleFence::refused_while_lapsed("stale-fence-kinds");
    let bound = sequence
        .claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_named_root_journal
        .iter()
        .find_map(|journal| journal.pending.clone())
        .unwrap();
    let ended = EngramNamedRootEvent {
        kind: EngramNamedRootKind::Ended,
        end_reason: Some(EngramNamedRootEndReason::ExplicitClear),
        ..bound.clone()
    };
    let read = |claim: Value| -> EngramNamedRootReadResponse {
        let mut read = json!({"project_id":"project", "work_id":"work", "root_execution_id":"root",
            "run_id":"run", "claim_id":"claim", "run":{"state":"claimed", "generation":1},
            "named_root":{"state":"none"}, "latest_event":null,
            "read_cut":{"feed":{"kind":"run_execution", "id":"run"}, "position":1}});
        if !claim.is_null() {
            read["claim"] = claim;
        }
        serde_json::from_value(read).unwrap()
    };
    let newer = read(json!({"state":"active", "fence":11}));
    assert!(newer.settles_intent(&bound));
    assert!(
        !newer.settles_intent(&ended),
        "an Ended isn't fence-checked, so it may still apply"
    );
    assert!(
        !read(json!({"state":"active", "fence":10})).settles_intent(&bound),
        "the same fence"
    );
    assert!(
        !read(json!({"state":"active"})).settles_intent(&bound),
        "no fence shown"
    );
    assert!(!read(Value::Null).settles_intent(&bound), "no claim shown");
}

fn naming_pending_root(claimed: &EOCheckedTurn) -> String {
    claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_named_root_journal
        .iter()
        .find_map(|journal| journal.pending.as_ref())
        .map(|event| event.root.root.clone())
        .expect("a pending name")
}

fn naming_pending_generation(claimed: &EOCheckedTurn) -> u64 {
    claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_named_root_journal
        .iter()
        .find_map(|journal| journal.pending.as_ref())
        .map(|event| event.root.generation)
        .expect("a pending name")
}
