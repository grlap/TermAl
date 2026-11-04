// Test-only child of src/tests/test_run_registration.rs. Owns the witnesses
// that an ordinary committed-input run's registration takes its work binding
// from the one named source root the launching session named for the run's
// worktree, with that claim's current held binding, and pins
// that root; that a stale session binding is never used for it; that such a
// run in another worktree, or under two matching names, stays unbound; and
// that a staged-lane run keeps copying the session's binding; and that the
// input kind joins the registration identity only when a request names one.
// Uses the parent's run and registration fixtures. Does not own the
// named-root lifecycle (src/tests/engram_source_roots.rs), observed commands,
// or the fence-time refusal of a flipped input (src/tests/test_run_phase_trace.rs).
// New module.
use super::*;

/// A fixed launcher request. Its identity below is the launcher's own digest
/// of these fields (`requestIdentity` in scripts/test-run-registration.mjs),
/// computed before the input kind joined the identity.
fn pinned_request() -> Value {
    json!({
        "runId": "test-identity-pinned", "root": "/repo", "full": true,
        "stages": [{ "name": "rust-tests" }], "owner": "session-one",
        "started": "2026-10-30T10:00:00.000Z", "creatorPid": 42, "detached": true,
    })
}

const PINNED_IDENTITY: &str = "b1b3be35621e7556ced036653b7314d47d953d2a0307c5d8fd8cefa42aed2731";

#[test]
fn a_request_without_input_keeps_its_unchanged_identity() {
    let request = pinned_request();
    assert_eq!(test_run_registration_identity(&request).unwrap(), PINNED_IDENTITY);
    // The unchanged path, recomputed: the fourteen immutable fields, null
    // when absent, in key order.
    let fields = [
        "runId", "root", "stages", "notifyTo", "full", "scope", "detached", "creatorPid",
        "owner", "started", "requiredBinaryEnv", "prerequisiteFiles", "needsCargo", "liveEngram",
    ];
    let unchanged: BTreeMap<_, _> = fields
        .into_iter()
        .map(|name| (name, request.get(name).cloned().unwrap_or(Value::Null)))
        .collect();
    assert_eq!(sha256_hex(&serde_json::to_vec(&unchanged).unwrap()), PINNED_IDENTITY);
}

#[test]
fn a_committed_input_request_has_a_different_identity() {
    let mut committed = pinned_request();
    committed["input"] = json!("commit");
    let identity = test_run_registration_identity(&committed).unwrap();
    assert_ne!(identity, PINNED_IDENTITY);
    let mut other = pinned_request();
    other["input"] = json!("tree");
    assert_ne!(test_run_registration_identity(&other).unwrap(), identity);
    assert_ne!(test_run_registration_identity(&other).unwrap(), PINNED_IDENTITY);
}

fn engram_owner(fixture: &RunFixture) -> (String, EngramAuthorityStoreKey) {
    let session = owner(fixture);
    let store = EngramAuthorityStoreKey {
        database_path: fixture.root.join("tracker.db"),
        project_id: "canonical-project".to_owned(),
    };
    let mut inner = fixture.state.inner.lock().unwrap();
    let project = inner
        .projects
        .iter_mut()
        .find(|project| project.id == fixture.project_id)
        .unwrap();
    project.engram = Some(EngramProjectSettings {
        enabled: true,
        authority_store_key: Some(store.clone()),
        ..EngramProjectSettings::default()
    });
    (session, store)
}

fn claim(label: &str) -> EngramControlWorkBinding {
    EngramControlWorkBinding {
        root_execution_id: format!("{label}-root"),
        work_id: format!("{label}-work"),
        run_id: format!("{label}-run"),
        work_revision: 3,
        claim_id: format!("{label}-claim"),
        claim_fence: 2,
    }
}

/// The fixture's main worktree as a named-root entry spells it.
fn canonical_root(fixture: &RunFixture) -> PathBuf {
    fs::canonicalize(&fixture.root).unwrap()
}

fn common_dir_key(fixture: &RunFixture) -> String {
    engram_exact_path_key(&fs::canonicalize(fixture.root.join(".git")).unwrap())
}

/// `session`'s name of `root` for `binding`'s claim, with the work's naming
/// history holding that run's canonical proof that the root is bound.
pub(super) fn name_root(
    fixture: &RunFixture,
    store: &EngramAuthorityStoreKey,
    binding: &EngramControlWorkBinding,
    session: &str,
    root: &FsPath,
    generation: u64,
) -> EngramWorkSourceRoot {
    let entry = EngramWorkSourceRoot {
        store: store.clone(),
        work_id: binding.work_id.clone(),
        short_ref: format!("{}-ref", binding.work_id),
        claim_id: binding.claim_id.clone(),
        claim_fence: binding.claim_fence,
        root: root.to_string_lossy().into_owned(),
        common_dir_key: common_dir_key(fixture),
        named_by_session: session.to_owned(),
        named_at: "2026-10-30T00:00:00.000Z".to_owned(),
        generation,
    };
    let feed = json!({ "kind": "run_execution", "id": binding.run_id });
    let read: EngramNamedRootReadResponse = serde_json::from_value(json!({
        "project_id": store.project_id, "work_id": binding.work_id, "run_id": binding.run_id,
        "claim_id": binding.claim_id, "root_execution_id": binding.root_execution_id,
        "run": { "state": "open", "generation": 2 },
        "named_root": { "state": "bound", "workspace_id": entry.root,
                        "generation": generation, "named_at": "2026-10-30T00:00:00Z" },
        "latest_event": null,
        "read_cut": { "feed": feed, "position": 1 },
    }))
    .unwrap();
    let mut inner = fixture.state.inner.lock().unwrap();
    inner.engram_work_source_roots.push(entry.clone());
    inner.engram_work_naming_history.push(EngramWorkNamingHistory {
        store: store.clone(),
        work_id: binding.work_id.clone(),
        revision: 1,
        known_generation: generation,
        epoch: 0,
        frontier: None,
        owner_version: 0,
        transition: None,
        unresolved_runs: Vec::new(),
        retired_runs: Vec::new(),
        proofs: vec![EngramAuthorityProof {
            binding: binding.clone(),
            read,
        }],
        recovery_reason: None,
        retirements: Vec::new(),
    });
    let bindings = inner.engram_work_naming_history.iter()
        .flat_map(|history| history.proofs.iter().map(|proof| EngramHeldClaim {
            work_id: proof.binding.work_id.clone(),
            short_ref: "named-work".to_owned(),
            claim_id: proof.binding.claim_id.clone(),
            claim_fence: proof.binding.claim_fence,
            focused: false,
            control_binding: Some(proof.binding.clone()),
        })).collect();
    let project = inner.projects.iter_mut().find(|project| project.id == fixture.project_id).unwrap();
    let mut settings = crate::tests::engram_host_adapter::real_fixture_engram_settings(&fixture.root);
    settings.authority_store_key = Some(store.clone());
    project.engram = Some(settings);
    drop(inner);
    let transport = ScriptedEngramControlTransport::new([]);
    transport.replace_held_claims([Ok(EngramHeldClaims { items: bindings, omitted: 0 })]);
    fixture.state.install_test_engram_transport(transport);
    entry
}

fn set_session_binding(fixture: &RunFixture, session: &str, binding: Option<EngramControlWorkBinding>) {
    let mut inner = fixture.state.inner.lock().unwrap();
    let index = inner.find_session_index(session).unwrap();
    inner.sessions[index].engram.work_binding = binding;
}

/// Registers `run_id`, an ordinary committed-input run when `committed`
/// (`"input": "commit"`), else a staged-lane run whose request names no input.
fn register_run(
    fixture: &RunFixture,
    session: &str,
    run_id: &str,
    committed: bool,
) -> TestRunRegistration {
    let directory = registered_run(fixture, session, run_id);
    if committed {
        let path = directory.join("request.json");
        let mut request: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        request["input"] = json!("commit");
        fs::write(&path, request.to_string()).unwrap();
    }
    fixture
        .state
        .register_test_run(session, registration_request(run_id, &directory))
        .unwrap();
    let inner = fixture.state.inner.lock().unwrap();
    inner
        .test_run_registrations
        .get(run_id)
        .cloned()
        .expect("the registration is recorded")
}

#[test]
fn a_committed_input_run_in_the_sessions_named_root_binds_its_claim_with_control_off() {
    let fixture = RunFixture::new("registration-named-root-binds");
    let (session, store) = engram_owner(&fixture);
    let binding = claim("named");
    let entry = name_root(&fixture, &store, &binding, &session, &canonical_root(&fixture), 1);
    // Turn-gated control OFF: the session itself holds no binding.
    set_session_binding(&fixture, &session, None);
    let registration = register_run(&fixture, &session, "test-named-root-binds", true);
    assert_eq!(registration.work_binding.as_ref(), Some(&binding));
    assert_eq!(
        registration.named_root,
        Some(TestRunNamedRoot {
            workspace_id: entry.root.clone(),
            generation: 1,
        })
    );
    assert_eq!(registration.authority_store.as_ref(), Some(&store));
    assert!(!registration.validated_unbound);
    assert_eq!(
        fixture
            .summary("test-named-root-binds")
            .registration
            .bound_work_ids,
        [binding.work_id.clone()]
    );

    // A registration persisted before the pinned root still loads, unpinned.
    let mut wire = serde_json::to_value(&registration).unwrap();
    assert_eq!(wire["namedRoot"]["workspaceId"], json!(entry.root));
    wire.as_object_mut().unwrap().remove("namedRoot");
    let older: TestRunRegistration = serde_json::from_value(wire).unwrap();
    assert_eq!(older.named_root, None);
    assert_eq!(older.work_binding, registration.work_binding);
}

#[test]
fn a_committed_input_run_ignores_a_stale_session_binding_for_another_claim() {
    let fixture = RunFixture::new("registration-named-root-stale");
    let (session, store) = engram_owner(&fixture);
    let stale = claim("stale");
    set_session_binding(&fixture, &session, Some(stale.clone()));
    let named = claim("named");
    name_root(&fixture, &store, &named, &session, &canonical_root(&fixture), 1);
    let registration = register_run(&fixture, &session, "test-stale-named", true);
    assert_eq!(registration.work_binding.as_ref(), Some(&named));
    assert_ne!(registration.work_binding.as_ref(), Some(&stale));
}

#[test]
fn a_committed_input_run_without_a_matching_named_root_stays_unbound() {
    let fixture = RunFixture::new("registration-named-root-unmatched");
    let (session, store) = engram_owner(&fixture);
    let binding = claim("elsewhere");
    // The session's own binding is never the fallback.
    set_session_binding(&fixture, &session, Some(binding.clone()));
    let unnamed = register_run(&fixture, &session, "test-committed-unnamed", true);
    assert_eq!(unnamed.work_binding, None, "no named root: unbound, not the session's claim");
    assert_eq!(unnamed.named_root, None);
    assert!(!unnamed.validated_unbound);

    // A name for another worktree, or by another session, does not match.
    name_root(
        &fixture,
        &store,
        &binding,
        &session,
        &canonical_root(&fixture).join("elsewhere"),
        1,
    );
    let other = claim("other-session");
    name_root(&fixture, &store, &other, "another-session", &canonical_root(&fixture), 2);
    let registration = register_run(&fixture, &session, "test-named-root-elsewhere", true);
    assert_eq!(registration.work_binding, None);
    assert_eq!(registration.named_root, None);
    assert!(
        fixture
            .summary("test-named-root-elsewhere")
            .registration
            .bound_work_ids
            .is_empty()
    );
}

#[test]
fn a_committed_input_run_under_two_matching_named_roots_stays_unbound() {
    let fixture = RunFixture::new("registration-named-root-two");
    let (session, store) = engram_owner(&fixture);
    let first = claim("first");
    let second = claim("second");
    name_root(&fixture, &store, &first, &session, &canonical_root(&fixture), 1);
    name_root(&fixture, &store, &second, &session, &canonical_root(&fixture), 2);
    set_session_binding(&fixture, &session, Some(first.clone()));
    let registration = register_run(&fixture, &session, "test-named-root-two", true);
    assert_eq!(registration.work_binding, None, "no pick by focus");
    assert_eq!(registration.named_root, None);
}

#[test]
fn a_staged_lane_run_still_copies_the_session_binding_beside_a_named_root() {
    let fixture = RunFixture::new("registration-named-root-staged");
    let (session, store) = engram_owner(&fixture);
    let session_binding = claim("session");
    let named = claim("named");
    // A different claim's name for this very worktree changes nothing here.
    name_root(&fixture, &store, &named, &session, &canonical_root(&fixture), 1);
    set_session_binding(&fixture, &session, Some(session_binding.clone()));
    let registration = register_run(&fixture, &session, "test-staged-copies", false);
    assert_eq!(registration.work_binding.as_ref(), Some(&session_binding));
    assert_eq!(registration.named_root, None);
    assert_eq!(registration.authority_store.as_ref(), Some(&store));
    assert!(!registration.validated_unbound);
    assert!(
        serde_json::to_value(&registration).unwrap().get("namedRoot").is_none(),
        "a staged-lane registration persists as before"
    );

    // With no session binding it stays unbound, the named root notwithstanding.
    set_session_binding(&fixture, &session, None);
    let unbound = register_run(&fixture, &session, "test-staged-unbound", false);
    assert_eq!(unbound.work_binding, None);
    assert_eq!(unbound.named_root, None);
}

fn current_held_claim(binding: &EngramControlWorkBinding) -> EngramHeldClaims {
    EngramHeldClaims {
        items: vec![EngramHeldClaim {
            work_id: binding.work_id.clone(),
            short_ref: "current-work".to_owned(),
            claim_id: binding.claim_id.clone(),
            claim_fence: binding.claim_fence,
            focused: true,
            control_binding: Some(binding.clone()),
        }],
        omitted: 0,
    }
}

#[test]
fn committed_registration_reads_the_revision_after_the_holder_revises() {
    let fixture = RunFixture::new("registration-current-revision");
    let (session, store) = engram_owner(&fixture);
    let original = claim("revised");
    name_root(&fixture, &store, &original, &session, &canonical_root(&fixture), 1);
    let transport = ScriptedEngramControlTransport::new([]);
    fixture.state.install_test_engram_transport(transport.clone());
    // The exact held claim has been revised, while its canonical naming
    // proof still carries the revision at which the root was named.
    let mut current = original.clone();
    current.work_revision += 1;
    let mut held = current_held_claim(&current);
    held.omitted = 1; // Omitted siblings do not invalidate the exact present row.
    transport.replace_held_claims([Ok(held)]);
    let registration = register_run(&fixture, &session, "test-revision-before-registration", true);
    assert_eq!(registration.work_binding.as_ref(), Some(&current));
    assert_eq!(transport.held_claim_reads(), 1);

    // A later revise must not rewrite the already registered association.
    // Engram compares that frozen revision to its current item when the
    // verification record arrives, so the intervening revision stays stale.
    let mut after_registration = current.clone();
    after_registration.work_revision += 1;
    transport.replace_held_claims([Ok(current_held_claim(&after_registration))]);
    let saved = fixture.state.inner.lock().unwrap().test_run_registrations
        .get("test-revision-before-registration").unwrap().clone();
    assert_eq!(saved.work_binding, registration.work_binding);
    assert_ne!(saved.work_binding.as_ref(), Some(&after_registration));
    let replay = register_run(&fixture, &session, "test-revision-before-registration", true);
    assert_eq!(replay.work_binding, registration.work_binding);
    assert_eq!(transport.held_claim_reads(), 1, "active replay never reads the current claim");
    let next = register_run(&fixture, &session, "test-revision-next-registration", true);
    assert_eq!(next.work_binding.as_ref(), Some(&after_registration));
    assert_eq!(transport.held_claim_reads(), 2);

    // Restore a retained identity in the same installed shape used after
    // compaction. Replay must still pin the first association, without RPC.
    let id = registration.run_id.clone();
    let summary = fixture.summary(&id);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        inner.test_run_registrations.remove(&id);
        assert!(!inner.test_runs.entries.iter().any(|row| row.summary.run_id == id),
            "registration fallback does not populate the scanner's index");
        inner.test_runs.compactions_durable.insert(id.clone());
        inner.test_run_compacted.insert(id.clone(), TestRunCompacted {
            compacted: true, request_identity: Some(registration.request_identity.clone()),
            summary, registration: Some(registration.clone()), observations: Vec::new(),
            request_stamp: None, results_stamp: None, reason: "fixture retained identity".to_owned(),
            artifact_conflict: false, artifact_unavailable: false,
        });
    }
    let directory = fixture.runs_dir().join(&id);
    fixture.state.register_test_run(&session, registration_request(&id, &directory)).unwrap();
    assert_eq!(transport.held_claim_reads(), 2, "compacted replay never reads the current claim");
    assert_eq!(fixture.state.inner.lock().unwrap().test_run_compacted[&id].registration.as_ref(), Some(&registration));
}

#[test]
fn committed_registration_never_falls_back_to_an_unavailable_or_different_claim() {
    let fixture = RunFixture::new("registration-current-claim-refusal");
    let (session, store) = engram_owner(&fixture);
    let original = claim("held");
    name_root(&fixture, &store, &original, &session, &canonical_root(&fixture), 1);
    let transport = ScriptedEngramControlTransport::new([]);
    fixture.state.install_test_engram_transport(transport.clone());
    let mut unavailable = current_held_claim(&original);
    unavailable.items[0].control_binding = None;
    let mut successor = original.clone();
    successor.claim_id = "successor-claim".to_owned();
    let mut other_run = original.clone();
    other_run.run_id = "another-run".to_owned();
    let mut disagreement = current_held_claim(&original);
    disagreement.items[0].claim_fence += 1;
    let mut regressed = original.clone();
    regressed.work_revision -= 1;
    let mut old_fence = original.clone();
    old_fence.claim_fence -= 1;
    let mut recovered = original.clone();
    recovered.claim_fence += 1;
    let mut duplicate = current_held_claim(&original);
    duplicate.items.push(duplicate.items[0].clone());
    let omitted = EngramHeldClaims { items: Vec::new(), omitted: 1 };
    for (index, (held, status, message)) in [
        (Ok(unavailable), StatusCode::CONFLICT, "held association changed"),
        (Ok(current_held_claim(&successor)), StatusCode::CONFLICT, "unavailable or ambiguous"),
        (Ok(current_held_claim(&other_run)), StatusCode::CONFLICT, "held association changed"),
        (Ok(disagreement), StatusCode::CONFLICT, "held association changed"),
        (Ok(current_held_claim(&regressed)), StatusCode::CONFLICT, "held association changed"),
        (Ok(current_held_claim(&old_fence)), StatusCode::CONFLICT, "held association changed"),
        (Ok(current_held_claim(&recovered)), StatusCode::CONFLICT, "name the source root again"),
        (Ok(duplicate), StatusCode::CONFLICT, "unavailable or ambiguous"),
        (Ok(omitted), StatusCode::CONFLICT, "bounded held list"),
        (Err(EngramTransportError::protocol("fixture held read failed")), StatusCode::BAD_GATEWAY, "fixture held read failed"),
    ]
        .into_iter().enumerate()
    {
        transport.replace_held_claims([held]);
        let run_id = format!("test-refused-current-claim-{index}");
        let directory = registered_run(&fixture, &session, &run_id);
        let path = directory.join("request.json");
        let mut request: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        request["input"] = json!("commit");
        fs::write(path, request.to_string()).unwrap();
        let error = fixture.state.register_test_run(&session, registration_request(&run_id, &directory)).err().expect("registration must refuse");
        assert_eq!(error.status, status, "case {index}: {}", error.message);
        assert!(error.message.contains(message), "case {index}: {}", error.message);
        assert!(!fixture.state.inner.lock().unwrap().test_run_registrations.contains_key(&run_id));
    }
}

#[test]
fn committed_registration_rechecks_the_named_root_after_the_held_read() {
    for fence_only in [false, true] {
        let fixture = RunFixture::new("registration-root-change-during-read");
        let (session, store) = engram_owner(&fixture);
        let original = claim("root-race");
        name_root(&fixture, &store, &original, &session, &canonical_root(&fixture), 1);
        let keys = test_run_worktree_keys(&fixture.root).unwrap();
        let clock = fixture.state.inner.lock().unwrap().engram_budget_clock_snapshot();
        let mut diagnostics = TestRunRegistrationDiagnostics::new("fixture");
        let prepared = fixture.state.read_test_run_current_named_binding(
            &session, "test-root-race", Some(&keys), &clock,
            test_run_registration_deadline(&clock, std::time::Instant::now()), &mut diagnostics).unwrap();
        let mut inner = fixture.state.inner.lock().unwrap();
        if fence_only { inner.engram_work_source_roots[0].claim_fence += 1; }
        else { inner.engram_work_source_roots[0].generation += 1; }
        let error = prepared.resolve_locked(&inner, &session, Some(&store), Some(&keys)).unwrap_err();
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert_eq!(error.message, "test run named root changed during its claim read");
    }
}

#[test]
fn an_expired_held_read_cannot_publish_a_late_registration() {
    for expiry in ["rpc", "rpc-error", "capture"] {
        let fixture = RunFixture::new("registration-held-read-budget");
        let (session, store) = engram_owner(&fixture);
        let binding = claim("budget");
        name_root(&fixture, &store, &binding, &session, &canonical_root(&fixture), 1);
        let clock = fixture.state.select_test_scripted_engram_budget_clock();
        fixture.state.inner.lock().unwrap().projects.iter_mut().find(|project| project.id == fixture.project_id)
            .unwrap().engram.as_mut().unwrap().deadline_ms = Some(20_000);
        let transport = ScriptedEngramControlTransport::new([]);
        let held = if expiry == "rpc-error" { Err(EngramTransportError::protocol("expired read error")) }
            else { Ok(current_held_claim(&binding)) };
        transport.replace_held_claims([held]);
        let state = fixture.state.clone();
        transport.on_next_held_claim_read(move |timeout| {
            assert!(state.inner.try_lock_now().is_some(), "held RPC must run outside StateInner");
            assert_eq!(timeout, Duration::from_secs(5), "the CLI shares the registration budget");
            if expiry == "capture" {
                clock.advance(Duration::from_secs(4));
                TEST_TEST_RUN_REGISTRATION_NEXT_LOCK.with(|slot| {
                    *slot.borrow_mut() = Some(Box::new(move || clock.advance(Duration::from_secs(1))));
                });
            } else {
                clock.advance(Duration::from_secs(5));
            }
        });
        fixture.state.install_test_engram_transport(transport.clone());
        let id = "test-held-read-deadline";
        let directory = registered_run(&fixture, &session, id);
        let path = directory.join("request.json");
        let mut request: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        request["input"] = json!("commit");
        fs::write(path, request.to_string()).unwrap();
        let error = fixture.state.register_test_run(&session, registration_request(id, &directory)).err().expect("expired registration must refuse");
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(error.message.contains("budget expired before publication"), "{}", error.message);
        assert!(error.message.contains("category=deadline"), "{}", error.message);
        assert_eq!(transport.held_claim_reads(), 1);
        assert!(!fixture.state.inner.lock().unwrap().test_run_registrations.contains_key(id));
    }
}

#[test]
fn committed_registration_rechecks_service_adapter_and_runtime_after_the_held_read() {
    for case in ["service", "adapter", "runtime"] {
        let fixture = RunFixture::new(&format!("registration-authority-race-{case}"));
        let (session, store) = engram_owner(&fixture);
        let binding = claim(case);
        name_root(&fixture, &store, &binding, &session, &canonical_root(&fixture), 1);
        let transport = ScriptedEngramControlTransport::new([]);
        transport.replace_held_claims([Ok(current_held_claim(&binding))]);
        let state = fixture.state.clone();
        let project = fixture.project_id.clone();
        let caller = session.clone();
        transport.on_next_held_claim_read(move |_| {
            if case == "adapter" {
                state.install_test_engram_transport(ScriptedEngramControlTransport::new([]));
            } else {
                let mut inner = state.inner.lock().unwrap();
                if case == "service" {
                    inner.projects.iter_mut().find(|row| row.id == project).unwrap()
                        .engram.as_mut().unwrap().deadline_ms = Some(251);
                } else {
                    let index = inner.find_session_index(&caller).unwrap();
                    let (runtime, _commands) = test_codex_runtime_handle("changed-during-held-read");
                    inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
                }
            }
        });
        fixture.state.install_test_engram_transport(transport);
        let id = "test-authority-race";
        let directory = registered_run(&fixture, &session, id);
        let path = directory.join("request.json");
        let mut request: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        request["input"] = json!("commit");
        fs::write(path, request.to_string()).unwrap();
        let error = fixture.state.register_test_run(&session, registration_request(id, &directory))
            .err().expect("changed authority must refuse");
        assert_eq!(error.status, StatusCode::CONFLICT, "{case}: {}", error.message);
        assert_eq!(error.message.split(';').next(), Some("test run work authority changed during its read"), "{case}");
        assert!(!fixture.state.inner.lock().unwrap().test_run_registrations.contains_key(id));
    }
}

#[test]
fn an_unbound_committed_run_keeps_its_store_with_an_unavailable_inventory() {
    let fixture = RunFixture::new("registration-unbound-inventory");
    let (session, store) = engram_owner(&fixture);
    fixture.state.inner.lock().unwrap().delegation_inventory = DelegationInventoryState::Failed { reason: "fixture" };
    let registration = register_run(&fixture, &session, "test-unbound-inventory", true);
    assert_eq!(registration.authority_store, Some(store));
    assert_eq!(registration.work_binding, None);
}

#[test]
fn concurrent_first_registrations_keep_the_first_inserted_association() {
    let fixture = RunFixture::new("registration-concurrent-claim");
    let (session, store) = engram_owner(&fixture);
    let original = claim("concurrent");
    name_root(&fixture, &store, &original, &session, &canonical_root(&fixture), 1);
    fixture.state.select_test_scripted_engram_budget_clock();
    let id = "test-concurrent-first-registration";
    let directory = registered_run(&fixture, &session, id);
    let path = directory.join("request.json");
    let mut request: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    request["input"] = json!("commit");
    fs::write(path, request.to_string()).unwrap();
    let transport = ScriptedEngramControlTransport::new([]);
    let mut first = original.clone();
    first.work_revision += 1;
    let mut second = first.clone();
    second.work_revision += 1;
    // The hook admits the competing POST before the original read returns.
    // Its held read consumes the first answer, then the original gets N+2.
    transport.replace_held_claims([Ok(current_held_claim(&first)), Ok(current_held_claim(&second))]);
    let state = fixture.state.clone();
    let caller = session.clone();
    let competing_directory = directory.clone();
    transport.on_next_held_claim_read(move |_| {
        state.register_test_run(&caller, registration_request(id, &competing_directory)).unwrap();
    });
    fixture.state.install_test_engram_transport(transport.clone());
    fixture.state.register_test_run(&session, registration_request(id, &directory)).unwrap();
    assert_eq!(transport.held_claim_reads(), 2);
    assert_eq!(fixture.state.inner.lock().unwrap().test_run_registrations[id].work_binding.as_ref(), Some(&first));
    fixture.state.register_test_run(&session, registration_request(id, &directory)).unwrap();
    assert_eq!(transport.held_claim_reads(), 2, "the subsequent replay reads neither association again");
}

#[test]
fn staged_and_unbound_runs_keep_their_existing_capture_deadline_behavior() {
    for committed in [false, true] {
        let fixture = RunFixture::new("registration-no-claim-read-deadline");
        let session = owner(&fixture);
        let clock = fixture.state.select_test_scripted_engram_budget_clock();
        TEST_TEST_RUN_REGISTRATION_NEXT_LOCK.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                // The first diagnostic lock resolves the project. The next
                // is capture; no new production clock lock intervenes.
                TEST_TEST_RUN_REGISTRATION_NEXT_LOCK.with(|next| {
                    *next.borrow_mut() = Some(Box::new(move || clock.advance(Duration::from_secs(5))));
                });
            }));
        });
        let saved = register_run(&fixture, &session, "test-no-held-read-deadline", committed);
        assert_eq!(saved.work_binding, None);
        assert_eq!(saved.named_root, None);
    }
}
