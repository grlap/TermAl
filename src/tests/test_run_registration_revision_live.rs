// Real disposable producer witnesses for registration's current revision.
// Owns holder revision and optional-child attachment before registration,
// and a later revision refusing an already frozen record. Uses the pinned
// completion fixture and its actual native Cargo check; never the live store.
use super::*;

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn registration_refreshes_after_revision_or_child_attachment_but_never_after_registration() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let local_tmp = normalize_user_facing_path(FsPath::new(env!("CARGO_MANIFEST_DIR"))).join(".tmp");
    for mutation in ["revision", "optional-child"] {
        let fixture = completion_fixture();
        assert!(fixture.home.starts_with(&local_tmp), "the disposable home belongs to this worktree");
        assert!(normalize_user_facing_path(&fixture.store().database_path).starts_with(&fixture.home),
            "the authority store belongs to the disposable home");
        let session = &fixture.live.session_id;
        let added = fixture.work(session, &["add", "Register the current parent revision",
            "--accept", "The registered native check passes", "--bind", "1=test", "--json"]);
        let work = added["work"]["short_ref"].as_str()
            .or_else(|| added["short_ref"].as_str()).unwrap();
        fixture.work(session, &["claim", work, "--json"]);
        let at_claim = fixture.claim_binding(session);
        let root = fixture.root.join(".worktrees/current-revision");
        run_git_test_command(&fixture.root, &["worktree", "add", "--quiet", "-b",
            "current-revision", root.to_str().unwrap()]);
        fixture.live.state.name_engram_source_root(session, EngramSourceRootRequest {
            work: work.to_owned(), path: Some(Some(root.to_string_lossy().into_owned())),
        }).unwrap();
        if mutation == "optional-child" {
            fixture.work(session, &["add", "Optional follow-up", "--under", work, "--optional", "--json"]);
        } else {
            fixture.work(session, &["update", work, "--outcome", "Check the revised parent contract", "--json"]);
        }
        let current = fixture.claim_binding(session);
        assert!(current.work_revision > at_claim.work_revision, "{mutation}");
        assert!(engram_same_recovery_run(&at_claim, &current));
        assert_eq!(current.claim_fence, at_claim.claim_fence);

        let named_proof = {
            let inner = fixture.live.state.inner.lock().unwrap();
            let entry = inner.engram_work_source_roots.iter().find(|entry| entry.work_id == at_claim.work_id).unwrap();
            engram_named_root_claim_binding(&inner.engram_work_naming_history, entry).unwrap()
        };
        assert_eq!(named_proof, at_claim, "the canonical naming proof still predates the planning mutation");
        // Exercise registered native artifacts directly, without introducing
        // a provider turn that might refresh that naming proof first.
        let (positive, frozen) = fixture.prepare_registered_check(session, &root, "cargo test --offline", false);
        let route = fixture.live.state.engram_verification_record_route(&positive).unwrap();
        let accepted = engram_verification_record_send(frozen, Ok(route.link()));
        let EngramVerificationRecordStep::Received { body, receipt } = &accepted else {
            panic!("the current revision must be accepted by the actual producer: {accepted:?}");
        };
        fixture.live.state.note_engram_verification_record_step(&positive, &accepted);
        assert_eq!(body.registration.binding, current, "{mutation}: registration refreshed");
        assert_eq!(receipt.verifications.len(), 1, "the real producer accepted the native record");

        // A separate real run is frozen before the next holder revision.
        // Its first send must be refused, rather than deduplicating an earlier
        // accepted record or silently replacing the registered association.
        let (registration, frozen) = fixture.prepare_registered_check(session, &root, "cargo test --offline", false);
        assert_eq!(frozen.body.registration.binding, current);
        fixture.work(session, &["update", work, "--outcome", "A newer contract after the check registered", "--json"]);
        let later = fixture.claim_binding(session);
        assert!(later.work_revision > current.work_revision);
        let route = fixture.live.state.engram_verification_record_route(&registration).unwrap();
        // The host's terminal detail currently retains only the refusal code.
        // Check the producer's original message as well, so an unrelated
        // authority refusal cannot satisfy the revision witness.
        let link = route.link();
        let error = link.adapter.request(link.connection, &engram_verification_record_request(&frozen), link.timeout)
            .expect_err("the actual producer refuses the stale frozen revision");
        assert_eq!(error.kind, EngramTransportErrorKind::Remote);
        assert_eq!(error.code.as_deref(), Some("verification_record_refused"));
        assert!(error.message.contains("work changed since registration"), "{error:?}");
        let step = engram_verification_record_send(frozen, Ok(route.link()));
        let EngramVerificationRecordStep::Rejected(detail) = &step else {
            panic!("the actual backend must refuse the stale registered revision: {step:?}");
        };
        assert_eq!(detail, "Engram refused it: verification_record_refused");
        fixture.live.state.note_engram_verification_record_step(&registration, &step);
        let stored = fixture.live.state.inner.lock().unwrap().test_run_registrations[&registration.run_id].clone();
        assert_eq!(stored.work_binding.as_ref(), Some(&current), "refusal never refreshes the registration");
        assert!(matches!(&stored.verification_record, Some(TestRunVerificationAck::Rejected { detail: saved }) if saved == detail),
            "the host retains the observed terminal refusal");
        assert!(matches!(engram_verification_record_prepare(&stored), EngramVerificationRecordPrepared::Finished),
            "the definite refusal is terminal rather than retried against newer work");
        assert_no_live_turn_gate(&fixture.live.transport);
    }
}
