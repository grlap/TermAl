//! A pending name whose claim was recovered, against a pinned real Engram: the
//! refusal, the fence in its read and the naming after it are all Engram's own.
//! The fence-checking test model and the rest of the cases stay in
//! engram_named_root_stale_fence.rs.
use super::*;

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn a_pending_name_under_a_superseded_fence_is_refused_and_no_longer_blocks_naming() {
    pending_name_under_a_superseded_fence(false);
}

/// With the work revised after the recovery, Engram has no record of that revision at
/// the old fence, so its refusal carries another code. The way out is the same.
#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn a_pending_name_refused_as_a_mismatch_after_a_revision_no_longer_blocks_naming() {
    pending_name_under_a_superseded_fence(true);
}

fn pending_name_under_a_superseded_fence(revised: bool) {
    let fixture = completion_fixture();
    let session = &fixture.live.session_id;
    let added = fixture.work(session, &["add", "Name a root across a claim recovery", "--json"]);
    let work = added["work"]["short_ref"].as_str().or_else(|| added["short_ref"].as_str()).unwrap();
    fixture.work(session, &["claim", work, "--json"]);
    let original = fixture.claim_binding(session);
    let worktree = |name: &str| {
        let path = fixture.root.join(".worktrees").join(name);
        run_git_test_command(&fixture.root, &[
            "worktree", "add", "--quiet", "-b", name, path.to_str().unwrap(),
        ]);
        path
    };
    let (first, other) = (worktree("first"), worktree("other"));
    let name = |path: &FsPath| fixture.live.state.name_engram_source_root(session, EngramSourceRootRequest {
        work: work.to_owned(),
        path: Some(Some(path.to_string_lossy().into_owned())),
    });

    // The first send never reaches Engram, so its intent is pending under this fence.
    fixture.live.transport.faults.lock().unwrap()
        .push_back(BoundaryFault::LoseBeforeSend { operation: "named_root_bind" });
    let lost = name(&first).unwrap_err();
    assert!(lost.message.contains("retry the same naming request"), "{lost:?}");

    // Released and claimed again: the same claim and run under a newer fence. A lapsed
    // claim claimed again by its holder goes through the same branch in Engram.
    fixture.work(session, &["update", work, "--release", "--reason", "Claim it again", "--json"]);
    fixture.work(session, &["claim", work, "--json"]);
    if revised {
        fixture.work(session, &["update", work, "--outcome", "Revised after the recovery", "--json"]);
    }
    let recovered = fixture.claim_binding(session);
    assert_eq!(recovered.run_id, original.run_id);
    assert_eq!(recovered.claim_id, original.claim_id);
    assert!(recovered.claim_fence > original.claim_fence);
    let read = fixture.canonical_read(session, &recovered);
    assert_eq!(read.claim.as_ref().and_then(|claim| claim.fence), Some(recovered.claim_fence));

    let blocked = name(&other).unwrap_err();
    assert!(blocked.message.contains("remains pending"), "{blocked:?}");
    let retried = name(&first).unwrap_err();
    if revised {
        assert!(retried.message.contains("work_claim_mismatch"), "{retried:?}");
    } else {
        assert!(retried.message.contains("named_root_binding_refused"), "{retried:?}");
        assert!(retried.message.contains("is stale"), "{retried:?}");
    }
    assert!(retried.message.contains("name a root again"), "{retried:?}");

    let named = name(&other).unwrap_or_else(|error| panic!("stuck behind the stale name: {error:?}"));
    let bound = fixture.canonical_read(session, &recovered);
    assert!(matches!(bound.named_root, EngramNamedRootState::Bound { generation, .. }
        if generation == named.generation as i64), "{:?}", bound.named_root);
    let sent = fixture.live.transport.requests_for("named_root_bind").into_iter()
        .map(|request| request["claim_fence"].as_i64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(sent, [original.claim_fence, recovered.claim_fence]);
}
