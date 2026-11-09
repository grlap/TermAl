//! Current holder authority is separate from immutable naming provenance.
use super::*;

fn committed_request(fixture: &RunFixture, session: &str, id: &str) -> RegisterTestRunRequest {
    let directory = registered_run(fixture, session, id);
    let path = directory.join("request.json");
    let mut request: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    request["input"] = json!("commit");
    fs::write(path, request.to_string()).unwrap();
    registration_request(id, &directory)
}

#[test]
fn current_held_association_keeps_original_name_and_registration_immutable() {
    let fixture = RunFixture::new("registration-current-holder");
    let (original_session, store) = engram_owner(&fixture);
    let binding = claim("handed-off");
    let entry = name_root(&fixture, &store, &binding, &original_session, &canonical_root(&fixture), 7);
    let original = register_run(&fixture, &original_session, "test-before-handoff", true);
    let original_wire = serde_json::to_value(&original).unwrap();
    let history = serde_json::to_value(&fixture.state.inner.lock().unwrap().engram_work_naming_history).unwrap();
    let new_holder = owner(&fixture);
    let transport = ScriptedEngramControlTransport::new([]);
    transport.replace_held_claims([Ok(current_held_claim(&binding))]);
    fixture.state.install_test_engram_transport(transport.clone());

    // Neither a missing association nor a stale focus may decide attribution.
    for (index, cached) in [None, Some(claim("unrelated-cache"))].into_iter().enumerate() {
        set_session_binding(&fixture, &new_holder, cached);
        let id = format!("test-new-holder-{index}");
        let request = committed_request(&fixture, &new_holder, &id);
        fixture.state.register_test_run(&new_holder, request).unwrap();
        let inner = fixture.state.inner.lock().unwrap();
        let registered = &inner.test_run_registrations[&id];
        assert_eq!(registered.work_binding.as_ref(), Some(&binding));
        assert_eq!(registered.initiating_session_id, new_holder);
        assert_eq!(registered.named_root.as_ref().unwrap().workspace_id, entry.root);
        assert_eq!(registered.named_root.as_ref().unwrap().generation, entry.generation);
        assert!(!registered.validated_unbound);
        assert_eq!(inner.engram_work_source_roots[0], entry);
        assert_eq!(serde_json::to_value(&inner.engram_work_naming_history).unwrap(), history);
        assert_eq!(serde_json::to_value(&inner.test_run_registrations["test-before-handoff"]).unwrap(), original_wire);
        // The native/declared observer's original-namer scope did not widen.
        let keys = test_run_worktree_keys(&fixture.root).unwrap();
        assert_eq!(test_run_registration_named_binding(&inner, &new_holder, Some(&store), Some(&keys)).unwrap(),
            (None, None));
        drop(inner);
        assert_eq!(fixture.summary(&id).registration.bound_work_ids, [binding.work_id.clone()]);
    }

    // The previous holder's new launch is refused, while retrying its original
    // admitted run retains the first immutable association without another read.
    transport.replace_held_claims([Ok(EngramHeldClaims { items: Vec::new(), omitted: 0 })]);
    let request = committed_request(&fixture, &original_session, "test-previous-holder");
    let error = fixture.state.register_test_run(&original_session, request).err().unwrap();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(!fixture.state.inner.lock().unwrap().test_run_registrations.contains_key("test-previous-holder"));
    let reads = transport.held_claim_reads();
    let request = committed_request(&fixture, &original_session, "test-before-handoff");
    fixture.state.register_test_run(&original_session, request).unwrap();
    assert_eq!(transport.held_claim_reads(), reads);
    assert_eq!(serde_json::to_value(&fixture.state.inner.lock().unwrap()
        .test_run_registrations["test-before-handoff"]).unwrap(), original_wire);
}

#[test]
fn foreign_original_name_needs_the_exact_current_held_run_and_fence() {
    let fixture = RunFixture::new("registration-holder-refusals");
    let (original_session, store) = engram_owner(&fixture);
    let binding = claim("held-exact");
    let entry = name_root(&fixture, &store, &binding, &original_session, &canonical_root(&fixture), 3);
    let new_holder = owner(&fixture);
    let transport = ScriptedEngramControlTransport::new([]);
    fixture.state.install_test_engram_transport(transport.clone());
    for case in ["absent", "claim", "run", "execution", "fence", "revision", "new-fence", "duplicate", "omitted", "error"] {
        let mut held = current_held_claim(&binding);
        match case {
            "absent" => held.items.clear(),
            "claim" => {
                held.items[0].claim_id.push_str("-successor");
                held.items[0].control_binding.as_mut().unwrap().claim_id = held.items[0].claim_id.clone();
            }
            "run" => held.items[0].control_binding.as_mut().unwrap().run_id.push_str("-foreign"),
            "execution" => held.items[0].control_binding.as_mut().unwrap().root_execution_id.push_str("-foreign"),
            "fence" => {
                held.items[0].claim_fence -= 1;
                held.items[0].control_binding.as_mut().unwrap().claim_fence -= 1;
            }
            "revision" => held.items[0].control_binding.as_mut().unwrap().work_revision -= 1,
            "new-fence" => {
                held.items[0].claim_fence += 1;
                held.items[0].control_binding.as_mut().unwrap().claim_fence += 1;
            }
            "duplicate" => held.items.push(held.items[0].clone()),
            "omitted" => { held.items.clear(); held.omitted = 1; }
            _ => {}
        }
        let response = if case == "error" { Err(EngramTransportError::protocol("holder read unavailable")) }
            else { Ok(held) };
        transport.replace_held_claims([response]);
        let id = format!("test-holder-refused-{case}");
        let request = committed_request(&fixture, &new_holder, &id);
        if matches!(case, "absent" | "claim") {
            fixture.state.register_test_run(&new_holder, request).unwrap();
            let inner = fixture.state.inner.lock().unwrap();
            assert!(inner.test_run_registrations[&id].work_binding.is_none());
            assert!(!inner.test_run_registrations[&id].validated_unbound);
            assert_eq!(inner.engram_work_source_roots[0], entry);
            continue;
        }
        let error = fixture.state.register_test_run(&new_holder, request).err()
            .unwrap_or_else(|| panic!("{case} borrowed another holder's root"));
        assert_eq!(error.status, if case == "error" { StatusCode::BAD_GATEWAY } else { StatusCode::CONFLICT }, "{case}: {error:?}");
        let inner = fixture.state.inner.lock().unwrap();
        assert!(!inner.test_run_registrations.contains_key(&id), "{case}");
        assert_eq!(inner.engram_work_source_roots[0], entry);
    }
}

#[test]
fn retained_foreign_name_requires_a_bound_live_canonical_proof() {
    for case in ["open", "completed", "cancelled", "released", "generation", "named-at", "root"] {
        let fixture = RunFixture::new(&format!("registration-holder-proof-{case}"));
        let (original_session, store) = engram_owner(&fixture);
        let binding = claim("proof-live");
        name_root(&fixture, &store, &binding, &original_session, &canonical_root(&fixture), 2);
        let new_holder = owner(&fixture);
        {
            let mut inner = fixture.state.inner.lock().unwrap();
            let proof = &mut inner.engram_work_naming_history[0].proofs[0].read;
            match case {
                "open" | "completed" | "cancelled" => proof.run.state = case.into(),
                "released" => proof.named_root = EngramNamedRootState::UnboundByRelease {
                    last_generation: 2, released_at_position: 9,
                },
                _ => if let EngramNamedRootState::Bound { workspace_id, generation, named_at } = &mut proof.named_root {
                    match case {
                        "generation" => *generation += 1,
                        "named-at" => named_at.push_str("-changed"),
                        "root" => workspace_id.push_str("-changed"),
                        _ => unreachable!(),
                    }
                },
            }
        }
        let request = committed_request(&fixture, &new_holder, "test-ended-name");
        let error = fixture.state.register_test_run(&new_holder, request).err()
            .unwrap_or_else(|| panic!("{case} borrowed a retired or different name"));
        assert_eq!(error.status, StatusCode::CONFLICT, "{case}: {error:?}");
        assert!(!fixture.state.inner.lock().unwrap().test_run_registrations.contains_key("test-ended-name"));
    }
}

#[test]
fn launcher_foreign_sibling_selection_distinguishes_absent_held_and_omitted() {
    for case in ["unheld", "held", "omitted"] {
        let fixture = RunFixture::new(&format!("registration-siblings-{case}"));
        let (session, store) = engram_owner(&fixture);
        let own = claim("own-sibling");
        let foreign = claim("foreign-sibling");
        name_root(&fixture, &store, &own, &session, &canonical_root(&fixture), 1);
        name_root(&fixture, &store, &foreign, "another-namer", &canonical_root(&fixture), 2);
        let mut held = current_held_claim(&own);
        if case == "held" { held.items.extend(current_held_claim(&foreign).items); }
        if case == "omitted" { held.omitted = 1; }
        let transport = ScriptedEngramControlTransport::new([]);
        transport.replace_held_claims([Ok(held)]);
        fixture.state.install_test_engram_transport(transport);
        let request = committed_request(&fixture, &session, "test-sibling-selection");
        if case == "unheld" {
            fixture.state.register_test_run(&session, request).unwrap();
            assert_eq!(fixture.state.inner.lock().unwrap().test_run_registrations["test-sibling-selection"]
                .work_binding.as_ref(), Some(&own));
        } else {
            let error = fixture.state.register_test_run(&session, request).err().unwrap();
            assert_eq!(error.status, StatusCode::CONFLICT);
            assert!(error.message.contains(if case == "held" { "ambiguous named roots" }
                else { "candidate ownership is absent from the bounded held list" }), "{case}: {error:?}");
            assert!(!fixture.state.inner.lock().unwrap().test_run_registrations.contains_key("test-sibling-selection"));
        }
    }
}

#[test]
fn native_reader_retains_original_namer_scope_despite_foreign_siblings() {
    for case in ["refresh", "omitted", "held"] {
        let fixture = RunFixture::new(&format!("native-reader-sibling-{case}"));
        let (session, store) = engram_owner(&fixture);
        let own = claim("native-own");
        let foreign = claim("native-foreign");
        let named = name_root(&fixture, &store, &own, &session, &canonical_root(&fixture), 1);
        name_root(&fixture, &store, &foreign, "another-namer", &canonical_root(&fixture), 2);
        let mut held = current_held_claim(&own);
        if case == "held" { held.items.extend(current_held_claim(&foreign).items); }
        if case == "omitted" { held.omitted = 1; }
        let transport = ScriptedEngramControlTransport::new([]);
        transport.replace_held_claims([Ok(held)]);
        if case == "refresh" {
            let state = fixture.state.clone();
            transport.on_next_held_claim_read(move |_| {
                state.inner.lock().unwrap().engram_work_source_roots[1].claim_fence += 1;
            });
        }
        fixture.state.install_test_engram_transport(transport);
        let keys = test_run_worktree_keys(&fixture.root).unwrap();
        let clock = fixture.state.inner.lock().unwrap().engram_budget_clock_snapshot();
        let mut diagnostics = TestRunRegistrationDiagnostics::new("native-sibling-fixture");
        // This is the existing reader called by declared opening and closing,
        // rather than the new launcher-specific admission path.
        let pending = fixture.state.read_test_run_current_named_binding(&session, "native-probe", Some(&keys),
            &clock, test_run_registration_deadline(&clock, std::time::Instant::now()), &mut diagnostics).unwrap();
        let inner = fixture.state.inner.lock().unwrap();
        let (binding, root) = pending.resolve_locked(&inner, &session, Some(&store), Some(&keys)).unwrap();
        assert_eq!(binding.as_ref(), Some(&own));
        assert_eq!(root.unwrap().generation, named.generation);
    }
}

#[test]
fn current_holder_registration_refuses_root_changes_during_the_held_read() {
    for case in ["removed", "root", "repository", "namer", "named-at", "generation", "fence"] {
        let fixture = RunFixture::new(&format!("registration-holder-race-{case}"));
        let (original_session, store) = engram_owner(&fixture);
        let binding = claim("holder-race");
        name_root(&fixture, &store, &binding, &original_session, &canonical_root(&fixture), 2);
        let new_holder = owner(&fixture);
        let transport = ScriptedEngramControlTransport::new([]);
        transport.replace_held_claims([Ok(current_held_claim(&binding))]);
        let state = fixture.state.clone();
        transport.on_next_held_claim_read(move |_| {
            let mut inner = state.inner.lock().unwrap();
            if case == "removed" { inner.engram_work_source_roots.clear(); return; }
            let entry = &mut inner.engram_work_source_roots[0];
            match case {
                "root" => entry.root.push_str("-replaced"),
                "repository" => entry.common_dir_key.push_str("-replaced"),
                "namer" => entry.named_by_session.push_str("-changed"),
                "named-at" => entry.named_at.push_str("-changed"),
                "generation" => entry.generation += 1,
                "fence" => entry.claim_fence += 1,
                _ => unreachable!(),
            }
        });
        fixture.state.install_test_engram_transport(transport);
        let request = committed_request(&fixture, &new_holder, "test-handoff-race");
        let error = fixture.state.register_test_run(&new_holder, request).err()
            .unwrap_or_else(|| panic!("{case} escaped the source-root snapshot"));
        assert_eq!(error.status, StatusCode::CONFLICT, "{case}: {error:?}");
        assert!(!fixture.state.inner.lock().unwrap().test_run_registrations.contains_key("test-handoff-race"));
    }
}
