// Test-only child of src/tests/test_run_registration.rs. Owns the witnesses
// that an ordinary committed-input run's registration takes its work binding
// from the one named source root the launching session named for the run's
// worktree, with that claim's binding from the work's naming proofs, and pins
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
        proofs: vec![EngramAuthorityProof {
            binding: binding.clone(),
            read,
        }],
        recovery_reason: None,
        retirements: Vec::new(),
    });
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
