//! A successful same-root name refreshes new registrations' claim authority;
//! it preserves the name and every registration already made under it.
use super::*;

#[test]
fn native_opening_after_claim_recovery_reaches_source_capture() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for sequence in ["fresh", "renewed", "retired-recovered", "retired-new-claim"] {
        let label = format!("native-opening-{sequence}");
        let turn = EOCheckedTurn::idle(&label, true);
        let path = naming_worktree(&turn.root);
        let original = naming_claim(&label);
        let first = request_naming(&turn, &label, Some(&path), vec![original.clone()]).unwrap();
        let mut held = original.clone();
        if sequence.starts_with("retired-") {
            let mut proof = {
                let inner = turn.state.inner.lock().unwrap();
                inner.engram_work_naming_history[0].proofs[0].read.clone()
            };
            proof.claim = Some(EngramNamedRootReadClaim {
                state: "active".into(), holder: Some(turn.session_id.clone()),
                expires_at: Some((chrono::Utc::now() - chrono::Duration::hours(3)).to_rfc3339()),
                revision: Some(original.items[0].control_binding.as_ref().unwrap().work_revision),
                fence: Some(original.items[0].claim_fence),
            });
            // A restored session no longer has a live work association. Feed
            // the canonical expired claim through the production retirement
            // pass, rather than deleting its root or naming history in place.
            turn.record_mut(|record| record.engram.work_binding = None);
            turn.transport.set_current_work_binding(None);
            turn.transport.replace_held_claims([Ok(no_held_claims())]);
            turn.transport.named_roots.lock().unwrap().as_mut().unwrap()
                .root_reads.insert(proof.claim_id.clone(), serde_json::to_value(&proof).unwrap());
            let plan = engram_root_reclamation_plan_locked(
                &turn.state.inner.lock().unwrap(), std::time::Instant::now());
            turn.state.run_engram_root_reclamation_pass(plan);
            assert!(turn.state.inner.lock().unwrap().engram_work_source_roots.is_empty(),
                "{sequence}: the expired generation must actually retire");
            turn.transport.named_roots.lock().unwrap().as_mut().unwrap()
                .root_reads.remove(&proof.claim_id);
        }
        if sequence != "fresh" {
            let claim = &mut held.items[0];
            claim.claim_fence += 1;
            let binding = claim.control_binding.as_mut().unwrap();
            binding.claim_fence = claim.claim_fence;
            binding.work_revision += 1;
            if sequence == "retired-new-claim" {
                binding.claim_id.push_str("-reclaimed");
                binding.run_id.push_str("-reclaimed");
                binding.root_execution_id.push_str("-reclaimed");
                claim.claim_id = binding.claim_id.clone();
            }
            turn.transport.set_current_work_binding(Some(binding.clone()));
            turn.transport.enable_named_roots(&turn.session_id, binding);
            turn.transport.named_roots.lock().unwrap().as_mut().unwrap()
                .claim_fences.insert(claim.claim_id.clone(), claim.claim_fence);
            turn.record_mut(|record| record.engram.work_binding = Some(binding.clone()));
            let second = request_naming(&turn, &label, Some(&path), vec![held.clone()]).unwrap();
            if sequence.starts_with("retired-") {
                assert!(second.generation > first.generation, "{sequence}");
            } else {
                assert_eq!(second.generation, first.generation, "same-root refresh keeps its name");
            }
        }
        let (entry_fence, proof_fence) = {
            let inner = turn.state.inner.lock().unwrap();
            let entry = &inner.engram_work_source_roots[0];
            let binding = engram_named_root_claim_binding(&inner.engram_work_naming_history, entry).unwrap();
            (entry.claim_fence, binding.claim_fence)
        };
        fs::write(path.join("termal-tests.toml"),
            "[[test]]\ncommand = \"dotnet test\"\ncwd = \".\"\nartifact = \".tmp/test.trx\"\n").unwrap();
        run_git_test_command(&path, &["add", "--", "termal-tests.toml"]);
        run_git_test_command(&path, &["commit", "--quiet", "-m", "Declare fixture command"]);
        let owner = crate::tests::engram_declared_source_fixture::NativeSourceOwner::new(&path);
        let project = turn.record(|record| record.session.project_id.clone().unwrap());
        owner.install(&turn.state, &project);
        let expected_source = owner.identity();
        let captures = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = captures.clone();
        turn.state.inner.lock().unwrap().test_declared_run_snapshot = Some(Arc::new(move |_, until| {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            owner.snapshot(until, None)
        }));
        turn.begin("Native opening authority diagnostic");
        let opening = turn.record(|record| record.engram.declared_run_turn.clone()).unwrap();
        eprintln!("native opening sequence={sequence} root_fence={entry_fence} proof_fence={proof_fence} refusal={:?}",
            opening.opening_refusal);
        assert_eq!(captures.load(std::sync::atomic::Ordering::SeqCst), 1,
            "{sequence}: valid current naming must reach source capture, refusal={:?}", opening.opening_refusal);
        assert_eq!(opening.snapshot.as_ref().map(|snapshot| &snapshot.source.source_id), Some(&expected_source),
            "{sequence}: S0 must publish only after the held claim is validated, refusal={:?}", opening.opening_refusal);
        assert_eq!(&opening.binding, held.items[0].control_binding.as_ref().unwrap(),
            "{sequence}: S0 must publish with the refreshed held binding");
        assert_eq!(opening.root_claim_fence, held.items[0].claim_fence);
    }
}

fn registration_request_for(
    turn: &EOCheckedTurn,
    root: &FsPath,
    run_id: &str,
) -> RegisterTestRunRequest {
    let common = test_runs_git_common_dir(root).expect("the named worktree has Git metadata");
    let directory = common.join("review-runs").join(run_id);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("request.json"), json!({
        "runId": run_id, "root": root.to_string_lossy(), "input": "commit",
        "full": true, "stages": [{"name": "rust-tests"}],
        "owner": turn.session_id, "creatorPid": std::process::id(),
        "detached": true, "started": "2026-11-05T07:00:00.000Z",
    }).to_string()).unwrap();
    RegisterTestRunRequest {
        run_id: run_id.to_owned(), run_directory: test_run_display_path(&directory),
    }
}

fn advance_held_fence(turn: &EOCheckedTurn, label: &str) -> EngramHeldClaims {
    let mut held = naming_claim(label);
    let claim = &mut held.items[0];
    claim.claim_fence += 1;
    let binding = claim.control_binding.as_mut().unwrap();
    binding.claim_fence = claim.claim_fence;
    binding.work_revision += 1;
    turn.transport.set_current_work_binding(Some(binding.clone()));
    turn.transport.named_roots.lock().unwrap().as_mut().unwrap()
        .claim_fences.insert(claim.claim_id.clone(), claim.claim_fence);
    turn.transport.replace_held_claims([Ok(held.clone())]);
    held
}

#[test]
fn a_new_holder_refreshes_the_same_name_before_registering_its_advanced_fence() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let label = "registration-real-handoff";
    let mut turn = EOCheckedTurn::idle(label, true);
    let path = naming_worktree(&turn.root);
    let initial = naming_claim(label);
    let named = request_naming(&turn, label, Some(&path), vec![initial.clone()]).unwrap();
    let old_session = turn.session_id.clone();
    let original_request = registration_request_for(&turn, &path, "test-before-real-handoff");
    let old_directory = original_request.run_directory.clone();
    turn.state.register_test_run(&old_session, original_request).unwrap();
    let (original, entry, event, mut read) = {
        let inner = turn.state.inner.lock().unwrap();
        (serde_json::to_value(&inner.test_run_registrations["test-before-real-handoff"]).unwrap(),
         inner.engram_work_source_roots[0].clone(),
         serde_json::to_value(&inner.engram_named_root_journal[0].confirmed).unwrap(),
         inner.engram_work_naming_history[0].proofs[0].read.clone())
    };
    assert_eq!(read.run.state, "claimed");
    let project = turn.record(|record| record.session.project_id.clone().unwrap());
    let successor = create_test_project_session(&turn.state, Agent::Codex, &project, &turn.root);
    let held = advance_held_fence(&turn, label);
    let binding = held.items[0].control_binding.as_ref().unwrap().clone();
    assert_eq!(binding.claim_fence, initial.items[0].claim_fence + 1);
    assert!(binding.work_revision > initial.items[0].control_binding.as_ref().unwrap().work_revision);
    turn.session_id = successor.clone();
    turn.record_mut(|record| record.engram.work_binding = Some(binding.clone()));
    turn.transport.enable_named_roots(&successor, &binding);
    // Model the producer's handoff-accepted result, not a second naming event.
    read.run.state = "active".into();
    read.read_cut.position += 1;
    read.claim = Some(EngramNamedRootReadClaim {
        state: "active".into(), holder: Some(successor.clone()),
        expires_at: Some((chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339()),
        revision: Some(binding.work_revision), fence: Some(binding.claim_fence),
    });
    turn.transport.named_roots.lock().unwrap().as_mut().unwrap()
        .root_reads.insert(binding.claim_id.clone(), serde_json::to_value(read).unwrap());

    let request = registration_request_for(&turn, &path, "test-before-new-holder-refresh");
    let error = turn.state.register_test_run(&successor, request).err().unwrap();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("claim fence changed; name the source root again"), "{error:?}");
    assert!(!turn.state.inner.lock().unwrap().test_run_registrations.contains_key("test-before-new-holder-refresh"));

    // The predecessor's scripted work-session reply cannot authorize the
    // successor's connection. Keep this refusal observable before supplying
    // the successor child's own identity, as a real child would return.
    let error = request_naming(&turn, label, Some(&path), vec![held.clone()]).err().unwrap();
    assert_eq!(error.status, StatusCode::BAD_GATEWAY);
    assert!(error.message.contains("ordinary work-session identity differs"), "{error:?}");
    assert_eq!(turn.state.inner.lock().unwrap().engram_work_source_roots[0], entry);
    turn.transport.named_roots.lock().unwrap().as_mut().unwrap().work_session_read_reply =
        Some(Ok(json!({
            "project_id": "github.com/example/source-root", "session_id": successor,
            "work_session": true, "capabilities": ["host_work_v1"],
        })));
    let refreshed = request_naming(&turn, label, Some(&path), vec![held]).unwrap();
    assert_eq!(refreshed.generation, named.generation);
    assert_eq!(refreshed.root, named.root);
    let request = registration_request_for(&turn, &path, "test-after-new-holder-refresh");
    turn.state.register_test_run(&successor, request).unwrap();
    let inner = turn.state.inner.lock().unwrap();
    let current = &inner.test_run_registrations["test-after-new-holder-refresh"];
    assert_eq!(current.work_binding.as_ref(), Some(&binding));
    assert_eq!(current.initiating_session_id, successor);
    assert_eq!(current.named_root.as_ref().unwrap().generation, named.generation);
    let retained = &inner.engram_work_source_roots[0];
    assert_eq!(retained.claim_fence, binding.claim_fence);
    assert_eq!(retained.named_by_session, old_session);
    assert_eq!(retained.named_at, entry.named_at);
    assert_eq!(retained.generation, entry.generation);
    assert_eq!(serde_json::to_value(&inner.engram_named_root_journal[0].confirmed).unwrap(), event);
    assert_eq!(serde_json::to_value(&inner.test_run_registrations["test-before-real-handoff"]).unwrap(), original);
    drop(inner);
    let saved = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    assert_eq!(saved.engram_work_source_roots[0].named_by_session, old_session);
    assert_eq!(saved.engram_work_source_roots[0].claim_fence, binding.claim_fence);
    let reads = turn.transport.held_claim_reads();
    turn.state.register_test_run(&old_session, RegisterTestRunRequest {
        run_id: "test-before-real-handoff".into(), run_directory: old_directory,
    }).unwrap();
    assert_eq!(turn.transport.held_claim_reads(), reads);
}

#[test]
fn naming_one_worktree_for_a_second_claim_reports_registration_ambiguity() {
    let label = "reg-two-names";
    let turn = EOCheckedTurn::idle(label, true);
    let path = naming_worktree(&turn.root);
    let mut held = naming_claim(label);
    let first_claim = held.items[0].clone();
    let first = request_naming(&turn, label, Some(&path), vec![held.clone()]).unwrap();
    let request = registration_request_for(&turn, &path, "test-before-second-name");
    turn.state.register_test_run(&turn.session_id, request).unwrap();
    let original = turn.state.inner.lock().unwrap()
        .test_run_registrations["test-before-second-name"].clone();
    assert_eq!(original.work_binding.as_ref(), first_claim.control_binding.as_ref());

    let second_binding = test_control_work_binding("second-reg", 1);
    held.items.push(EngramHeldClaim {
        work_id: second_binding.work_id.clone(), short_ref: "w-second-reg".to_owned(),
        claim_id: second_binding.claim_id.clone(), claim_fence: second_binding.claim_fence,
        focused: false, control_binding: Some(second_binding.clone()),
    });
    turn.transport.replace_held_claims([Ok(held.clone())]);
    turn.transport.register_named_root_run(&second_binding);
    let second = turn.state.name_engram_source_root(&turn.session_id, EngramSourceRootRequest {
        work: "w-second-reg".to_owned(), path: Some(Some(path.to_string_lossy().into_owned())),
    }).unwrap();
    assert_ne!(first.generation, second.generation);
    assert_eq!(turn.state.inner.lock().unwrap().engram_work_source_roots.len(), 2);

    // Replaying an already admitted request preserves its original association.
    let replay = registration_request_for(&turn, &path, "test-before-second-name");
    turn.state.register_test_run(&turn.session_id, replay).unwrap();
    assert_eq!(turn.state.inner.lock().unwrap()
        .test_run_registrations["test-before-second-name"].work_binding, original.work_binding);

    // Neither the current session binding nor focus may silently pick a name.
    for (index, binding) in [None, first_claim.control_binding.clone(), Some(second_binding.clone())]
        .into_iter().enumerate()
    {
        turn.record_mut(|record| record.engram.work_binding = binding);
        let id = format!("test-ambiguous-name-{index}");
        let request = registration_request_for(&turn, &path, &id);
        let error = turn.state.register_test_run(&turn.session_id, request)
            .err().expect("two retained names must not produce a silently unbound registration");
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(error.message.contains("ambiguous"), "{error:?}");
        for (work, claim, generation) in [
            (&first_claim.work_id, &first_claim.claim_id, first.generation),
            (&second_binding.work_id, &second_binding.claim_id, second.generation),
        ] {
            assert!(error.message.contains(work), "{error:?}");
            assert!(error.message.contains(claim), "{error:?}");
            assert!(error.message.contains(&format!("generation {generation}")), "{error:?}");
        }
        assert!(!turn.state.inner.lock().unwrap().test_run_registrations.contains_key(&id));
    }

    // The supported own-clear removes only the earlier name. The remaining
    // single entry registers normally without relabelling the original run.
    request_naming(&turn, label, None, vec![held]).unwrap();
    let request = registration_request_for(&turn, &path, "test-after-earlier-clear");
    turn.state.register_test_run(&turn.session_id, request).unwrap();
    let inner = turn.state.inner.lock().unwrap();
    let current = &inner.test_run_registrations["test-after-earlier-clear"];
    assert_eq!(current.work_binding.as_ref(), Some(&second_binding));
    assert_eq!(current.named_root.as_ref().unwrap().generation, second.generation);
    assert_eq!(serde_json::to_value(&inner.test_run_registrations["test-before-second-name"]).unwrap(),
        serde_json::to_value(original).unwrap());
}

#[test]
fn same_root_refresh_allows_current_fence_registration_without_rewriting_history() {
    let label = "reg-current-fence";
    let turn = EOCheckedTurn::idle(label, true);
    let path = naming_worktree(&turn.root);
    let named = request_naming(&turn, label, Some(&path), vec![naming_claim(label)]).unwrap();
    let original_request = registration_request_for(&turn, &path, "test-refresh-original");
    turn.state.register_test_run(&turn.session_id, original_request).unwrap();
    let (original, original_entry, original_event) = {
        let inner = turn.state.inner.lock().unwrap();
        (serde_json::to_value(&inner.test_run_registrations["test-refresh-original"]).unwrap(),
         inner.engram_work_source_roots[0].clone(),
         serde_json::to_value(&inner.engram_named_root_journal[0].confirmed).unwrap())
    };

    // Only the producer's live claim changes. No host root entry, session
    // association or registration is repaired by this fixture.
    let held = advance_held_fence(&turn, label);
    let request = registration_request_for(&turn, &path, "test-refresh-current");
    let refused = turn.state.register_test_run(&turn.session_id, RegisterTestRunRequest {
        run_id: request.run_id.clone(), run_directory: request.run_directory.clone(),
    }).err().expect("registration must refuse the stale retained fence");
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert!(refused.message.contains("claim fence changed"), "{refused:?}");
    assert!(!turn.state.inner.lock().unwrap().test_run_registrations
        .contains_key("test-refresh-current"));

    let refreshed = request_naming(&turn, label, Some(&path), vec![held.clone()]).unwrap();
    assert_eq!(refreshed.generation, named.generation);
    assert_eq!(refreshed.root, named.root);
    turn.state.register_test_run(&turn.session_id, request)
        .expect("the prescribed same-root refresh must make current registration possible");
    let inner = turn.state.inner.lock().unwrap();
    let current = &inner.test_run_registrations["test-refresh-current"];
    assert_eq!(current.work_binding.as_ref(), held.items[0].control_binding.as_ref());
    assert_eq!(current.named_root.as_ref().unwrap().generation, named.generation);
    assert_eq!(current.authority_store, Some(original_entry.store.clone()));
    let entry = engram_work_source_root_for_claim(&inner.engram_work_source_roots,
        &original_entry.store, &original_entry.work_id, &original_entry.claim_id).unwrap();
    assert_eq!(entry.claim_fence, held.items[0].claim_fence);
    assert_eq!(entry.named_at, original_entry.named_at);
    assert_eq!(serde_json::to_value(&inner.test_run_registrations["test-refresh-original"]).unwrap(),
        original, "a new registration must not relabel historical evidence");
    assert_eq!(serde_json::to_value(&inner.engram_named_root_journal[0].confirmed).unwrap(),
        original_event, "the original naming event stays immutable");
    drop(inner);
    let saved = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    assert_eq!(saved.engram_work_source_roots[0].claim_fence, held.items[0].claim_fence);
}

#[test]
fn refreshed_registration_still_refuses_missing_foreign_and_stale_claims() {
    let label = "registration-refresh-refusals";
    let turn = EOCheckedTurn::idle(label, true);
    let path = naming_worktree(&turn.root);
    request_naming(&turn, label, Some(&path), vec![naming_claim(label)]).unwrap();
    let held = advance_held_fence(&turn, label);
    let named = request_naming(&turn, label, Some(&path), vec![held.clone()]).unwrap();
    let baseline = registration_request_for(&turn, &path, "test-refresh-negative-baseline");
    turn.state.register_test_run(&turn.session_id, baseline).unwrap();
    let original = serde_json::to_value(&turn.state.inner.lock().unwrap()
        .test_run_registrations["test-refresh-negative-baseline"]).unwrap();

    let mut displaced = held.clone();
    displaced.items[0].claim_id.push_str("-successor");
    displaced.items[0].control_binding.as_mut().unwrap().claim_id =
        displaced.items[0].claim_id.clone();
    let mut foreign_work = held.clone();
    foreign_work.items[0].work_id.push_str("-foreign");
    foreign_work.items[0].control_binding.as_mut().unwrap().work_id =
        foreign_work.items[0].work_id.clone();
    let mut foreign_run = held.clone();
    foreign_run.items[0].control_binding.as_mut().unwrap().run_id.push_str("-foreign");
    let mut foreign_root_execution = held.clone();
    foreign_root_execution.items[0].control_binding.as_mut().unwrap()
        .root_execution_id.push_str("-foreign");
    let mut stale_fence = held.clone();
    stale_fence.items[0].claim_fence -= 1;
    stale_fence.items[0].control_binding.as_mut().unwrap().claim_fence -= 1;
    let mut stale_revision = held.clone();
    stale_revision.items[0].control_binding.as_mut().unwrap().work_revision -= 1;
    let mut disagreement = held.clone();
    disagreement.items[0].claim_fence += 1;
    for (case, unavailable, reason) in [
        ("released", no_held_claims(), "exact held claim is unavailable or ambiguous"),
        ("displaced", displaced, "exact held claim is unavailable or ambiguous"),
        ("foreign-work", foreign_work, "exact held claim is unavailable or ambiguous"),
        ("foreign-run", foreign_run, "held association changed"),
        ("foreign-root-execution", foreign_root_execution, "held association changed"),
        ("stale-fence", stale_fence, "held association changed"),
        ("stale-revision", stale_revision, "held association changed"),
        ("fence-disagreement", disagreement, "held association changed"),
    ] {
        // Vary only the canonical held reply after a real successful refresh.
        // Each case must refuse independently, then admit when restored.
        turn.transport.replace_held_claims([Ok(unavailable)]);
        let id = format!("test-refresh-refused-{case}");
        let request = registration_request_for(&turn, &path, &id);
        let error = turn.state.register_test_run(&turn.session_id, request)
            .err().unwrap_or_else(|| panic!("{case} borrowed refreshed authority"));
        assert_eq!(error.status, StatusCode::CONFLICT, "{case}: {error:?}");
        assert!(error.message.contains(reason), "{case}: {error:?}");
        assert!(!turn.state.inner.lock().unwrap().test_run_registrations.contains_key(&id));

        turn.transport.replace_held_claims([Ok(held.clone())]);
        let restored_id = format!("test-refresh-restored-{case}");
        let request = registration_request_for(&turn, &path, &restored_id);
        turn.state.register_test_run(&turn.session_id, request).unwrap();
        let inner = turn.state.inner.lock().unwrap();
        let restored = &inner.test_run_registrations[&restored_id];
        assert_eq!(restored.work_binding.as_ref(), held.items[0].control_binding.as_ref());
        assert_eq!(restored.named_root.as_ref().unwrap().generation, named.generation);
        assert_eq!(serde_json::to_value(&inner.test_run_registrations[
            "test-refresh-negative-baseline"]).unwrap(), original);
    }
}

#[test]
fn a_released_name_cannot_keep_the_refreshed_generation() {
    let label = "refresh-release";
    let turn = EOCheckedTurn::idle(label, true);
    let path = naming_worktree(&turn.root);
    request_naming(&turn, label, Some(&path), vec![naming_claim(label)]).unwrap();
    let held = advance_held_fence(&turn, label);
    let named = request_naming(&turn, label, Some(&path), vec![held.clone()]).unwrap();
    let request = registration_request_for(&turn, &path, "test-refresh-before-release");
    turn.state.register_test_run(&turn.session_id, request).unwrap();
    let original = serde_json::to_value(&turn.state.inner.lock().unwrap()
        .test_run_registrations["test-refresh-before-release"]).unwrap();

    // The producer records a genuine release; a held recovery of the same
    // claim is allowed to name again, but it must create a new generation.
    let mut recovered = held.clone();
    let claim = &mut recovered.items[0];
    claim.claim_fence += 2;
    let binding = claim.control_binding.as_mut().unwrap();
    binding.claim_fence = claim.claim_fence;
    binding.work_revision += 1;
    turn.transport.set_current_work_binding(Some(binding.clone()));
    {
        let mut roots = turn.transport.named_roots.lock().unwrap();
        let roots = roots.as_mut().unwrap();
        roots.released.insert(recovered.items[0].claim_id.clone(), named.generation as i64);
        roots.claim_fences.insert(recovered.items[0].claim_id.clone(), recovered.items[0].claim_fence);
    }
    let renamed = request_naming(&turn, label, Some(&path), vec![recovered.clone()]).unwrap();
    assert!(renamed.generation > named.generation);
    let request = registration_request_for(&turn, &path, "test-refresh-after-release");
    turn.state.register_test_run(&turn.session_id, request).unwrap();
    let inner = turn.state.inner.lock().unwrap();
    let current = &inner.test_run_registrations["test-refresh-after-release"];
    assert_eq!(current.named_root.as_ref().unwrap().generation, renamed.generation);
    assert_eq!(current.work_binding.as_ref(), recovered.items[0].control_binding.as_ref());
    assert_eq!(serde_json::to_value(&inner.test_run_registrations[
        "test-refresh-before-release"]).unwrap(), original);
}

#[test]
fn a_refreshed_name_stays_confirmed_when_the_parent_leaves_its_claim() {
    let label = "refresh-eval";
    let turn = EOCheckedTurn::idle(label, true);
    let path = naming_worktree(&turn.root);
    let held = naming_claim(label);
    let original_binding = held.items[0].control_binding.clone().unwrap();
    request_naming(&turn, label, Some(&path), vec![held]).unwrap();
    let store = naming_store(&turn);
    let claim = AcceptanceEvaluationSourceClaim {
        work_id: original_binding.work_id.clone(),
        claim_id: original_binding.claim_id.clone(),
        named_generation_at_request: None,
    };
    let assert_confirmed = || {
        let inner = turn.state.inner.lock().unwrap();
        assert!(!acceptance_evaluation_claim_unconfirmed_locked(
            &inner, &turn.session_id, &store, Some(&claim)),
            "the requested claim's acknowledged name must remain confirmed after focus moves");
    };
    // Establish that the unbound-parent branch admits this name before the
    // refresh. Only the session association changes; no root or event is repaired.
    turn.record_mut(|record| record.engram.work_binding = None);
    assert_confirmed();
    turn.record_mut(|record| record.engram.work_binding = Some(original_binding));
    let held = advance_held_fence(&turn, label);
    request_naming(&turn, label, Some(&path), vec![held]).unwrap();
    for binding in [None, Some(test_control_work_binding("other-eval-claim", 1))] {
        turn.record_mut(|record| record.engram.work_binding = binding);
        // Exercise the actual evaluator-admission predicate without spawning
        // an evaluator or requesting a live evaluation under the OFF policy.
        assert_confirmed();
    }

    let selected = turn.state.inner.lock().unwrap().engram_work_source_roots[0].clone();
    for field in ["store-database", "store-project", "work", "short-ref", "claim", "path",
        "repository", "namer", "named-at", "generation"] {
        let mut changed = selected.clone();
        match field {
            "store-database" => changed.store.database_path.push("foreign"),
            "store-project" => changed.store.project_id.push_str("-foreign"),
            "work" => changed.work_id.push_str("-foreign"),
            "short-ref" => changed.short_ref.push_str("-foreign"),
            "claim" => changed.claim_id.push_str("-foreign"),
            "path" => changed.root.push_str("-foreign"),
            "repository" => changed.common_dir_key.push_str("-foreign"),
            "namer" => changed.named_by_session.push_str("-foreign"),
            "named-at" => changed.named_at.push_str("-foreign"),
            "generation" => changed.generation += 1,
            _ => unreachable!(),
        }
        {
            let mut inner = turn.state.inner.lock().unwrap();
            inner.engram_work_source_roots[0] = changed;
            assert!(acceptance_evaluation_claim_unconfirmed_locked(
                &inner, &turn.session_id, &store, Some(&claim)),
                "a changed {field} must not borrow the acknowledged name");
            inner.engram_work_source_roots[0] = selected.clone();
        }
        assert_confirmed();
    }
}
