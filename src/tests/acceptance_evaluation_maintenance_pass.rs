// Owns request-opening classification of same-binding maintenance and recovery
// of an exact retained Candidate before the definitive evidence bracket.
// Reuses the acceptance and authority fixtures; does not own maintenance
// scheduling, source sightings, submission retry or timeout policy.
use super::*;

struct Maintenance {
    state: AppState,
    parent: String,
    store: EngramAuthorityStoreKey,
    binding: EngramControlWorkBinding,
    proof: EngramNamedRootReadResponse,
}

impl Maintenance {
    fn new() -> Self {
        let (state, project, parent, root) = fixture();
        install_store(&state, &project, &root);
        super::source::make_workdir_a_worktree(&root);
        let store = established_store(&state, &project).unwrap();
        let (binding, mut proof) = {
            let inner = state.inner.lock().unwrap();
            let evidence = &inner.engram_work_naming_history[0].proofs[0];
            (evidence.binding.clone(), evidence.read.clone())
        };
        let (named_root, common_dir_key) = validate_engram_source_root(
            &root.to_string_lossy(), &root.to_string_lossy(), &root.to_string_lossy(),
        ).unwrap();
        let named_at = "2026-09-27T00:00:00.000Z".to_owned();
        let entry = EngramWorkSourceRoot {
            store: store.clone(), work_id: binding.work_id.clone(),
            short_ref: "w-task".to_owned(), claim_id: binding.claim_id.clone(),
            claim_fence: binding.claim_fence, root: named_root.clone(), common_dir_key,
            named_by_session: parent.clone(), named_at: named_at.clone(), generation: 1,
        };
        let named = EngramNamedRootState::Bound {
            workspace_id: named_root.clone(), generation: 1, named_at: named_at.clone(),
        };
        proof.named_root = named.clone();
        proof.read_cut.position = 1;
        proof.latest_event = Some(EngramNamedRootReadEvent {
            event: "fixture-bound".to_owned(), position: proof.read_cut.clone(),
            generation: 1, kind: EngramNamedRootKind::Bound,
            workspace_id: named_root.clone(), named_at,
        });
        {
            let mut inner = state.inner.lock().unwrap();
            inner.engram_work_source_roots.push(entry.clone());
            inner.engram_named_root_journal.push(EngramNamedRootJournal {
                store: store.clone(), claim_id: binding.claim_id.clone(),
                read_binding: Some(binding.clone()),
                confirmed: Some((EngramNamedRootEvent {
                    root: entry.clone(), reporter: parent.clone(),
                    kind: EngramNamedRootKind::Bound, end_reason: None,
                }, EngramNamedRootReceipt {
                    event: "fixture-bound".to_owned(), position: proof.read_cut.clone(),
                    workspace_id: named_root.clone(), generation: 1,
                    kind: EngramNamedRootKind::Bound,
                })),
                pending: None, obsolete: false, retirement: None, reconciliation: None,
            });
            let index = inner.find_session_index(&parent).unwrap();
            inner.sessions[index].engram.work_binding = Some(binding.clone());
            inner.sessions[index].engram.named_root = Some(named);
        }
        let owner = state.prepare_engram_authority(&store, &binding, Duration::from_secs(2)).unwrap();
        AppState::learn_engram_authority_locked(&mut state.inner.lock().unwrap(), &store, &owner, &proof).unwrap();
        state.publish_engram_authority(&store, &owner, Duration::from_secs(2)).unwrap();
        super::sighting::install_sighting_transport(
            &state, &named_root, 1, super::sighting::present_sighting(),
        );
        Self { state, parent, store, binding, proof }
    }

    fn prepare(&self) -> EngramAuthorityTransition {
        self.state.prepare_engram_authority(&self.store, &self.binding, Duration::from_secs(2)).unwrap()
    }

    fn request(&self) -> Result<AcceptanceEvaluationRequestResponse, ApiError> {
        self.request_with(evidence_selection::canonical_core_receipt(), &self.binding.claim_id)
    }

    fn request_with(&self, core: Value, claim_id: &str) -> Result<AcceptanceEvaluationRequestResponse, ApiError> {
        let base = fixture_reader(Arc::default(), show_receipt(Some("same_session")),
            Ok(policy_receipt(Some(&["same_session"]))));
        let clock = self.state.engram_budget_clock();
        self.state.request_acceptance_evaluation_until(
            &self.parent, evaluation_request(None),
            |connection, args, timeout| {
                if args.iter().any(|arg| arg == "inspect") {
                    Ok(core.clone())
                } else if args.iter().any(|arg| arg == "held") {
                    Ok(json!({"items":[{"work_id":self.binding.work_id,
                        "short_ref":"w-task", "claim_id":claim_id,
                        "claim_fence":self.binding.claim_fence}], "omitted":0}))
                } else { base(connection, args, timeout) }
            },
            clock.now() + acceptance_evaluation_request_tracker_budget(), || clock.now(),
        )
    }

    fn strand_candidate(&self) -> EngramAuthorityTransition {
        let owner = self.prepare();
        AppState::learn_engram_authority_locked(&mut self.state.inner.lock().unwrap(),
            &self.store, &owner, &self.proof).unwrap();
        let clock = self.state.engram_budget_clock();
        let injected = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = injected.clone();
        self.state.inner.lock().unwrap().test_engram_authority_ack_boundary = Some(Arc::new(move |phase| {
            if phase == "before_publication" {
                observed.store(true, std::sync::atomic::Ordering::SeqCst);
                clock.advance(ENGRAM_ROOT_RECLAMATION_BUDGET + Duration::from_secs(1));
            }
        }));
        let result = self.state.publish_engram_authority(&self.store, &owner, ENGRAM_ROOT_RECLAMATION_BUDGET);
        self.state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;
        assert!(injected.load(std::sync::atomic::Ordering::SeqCst));
        assert!(result.is_err(), "the publication operation exceeded its own deadline");
        let inner = self.state.inner.lock().unwrap();
        let history = &inner.engram_work_naming_history[0];
        assert_eq!(history.transition.as_ref().unwrap().phase, EngramAuthorityPhase::Candidate);
        assert!(history.recovery_reason.is_none());
        assert!(history.unresolved_runs.is_empty());
        owner
    }
}

#[test]
fn a_settled_requested_binding_is_evaluated() {
    let fixture = Maintenance::new();
    assert!(matches!(fixture.request().unwrap(), AcceptanceEvaluationRequestResponse::SameSession { .. }));
}

#[test]
fn an_exact_same_binding_prepared_pass_is_retryable_not_a_final_conflict() {
    let fixture = Maintenance::new();
    fixture.prepare();
    {
        let inner = fixture.state.inner.lock().unwrap();
        let history = &inner.engram_work_naming_history[0];
        let entry = &inner.engram_work_source_roots[0];
        let bound = EngramWorkNamingBound::of_entry(entry, history).unwrap();
        assert!(engram_work_authority_transition_in_flight(&inner, &fixture.store,
            &fixture.binding.work_id, Some(&bound)), "the exact existing predicate applies");
    }
    let error = fixture.request().err().expect("Prepared is not force-published");
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE, "{}", error.message);
    assert!(error.message.starts_with(ACCEPTANCE_EVALUATION_AUTHORITY_IN_FLIGHT));
    assert!(error.message.contains(&format!("again in {} seconds", ACCEPTANCE_EVALUATION_IN_FLIGHT_RETRY_AFTER.as_secs())));
    let inner = fixture.state.inner.lock().unwrap();
    assert!(inner.delegations.is_empty());
    assert_eq!(inner.engram_work_naming_history[0].transition.as_ref().unwrap().phase, EngramAuthorityPhase::Prepared);
}

#[test]
fn one_request_recovers_the_exact_candidate_before_its_definitive_opening() {
    let fixture = Maintenance::new();
    let owner = fixture.strand_candidate();
    let before = fixture.state.inner.lock().unwrap().engram_source_root_generation;
    assert!(matches!(fixture.request().unwrap(), AcceptanceEvaluationRequestResponse::SameSession { .. }));
    let inner = fixture.state.inner.lock().unwrap();
    let history = &inner.engram_work_naming_history[0];
    let recovered = history.transition.as_ref().unwrap();
    assert_eq!((&recovered.id, recovered.version), (&owner.id, owner.version));
    assert_eq!(recovered.phase, EngramAuthorityPhase::Published);
    assert_eq!(inner.engram_source_root_generation, before);
    assert!(!engram_authority_work_unresolved(&inner, &fixture.store, &fixture.binding.work_id));
    assert!(inner.delegations.is_empty());
}

#[test]
fn an_unconfirmed_candidate_never_admits_an_evaluator() {
    let fixture = Maintenance::new();
    let owner = fixture.strand_candidate();
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(false));
    let result = fixture.request();
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(true));
    let error = result.err().expect("unconfirmed recovery must refuse the request");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR, "{}", error.message);
    assert_eq!(error.message, "named-root authority writer stopped; recovery remains withheld");
    let inner = fixture.state.inner.lock().unwrap();
    assert!(inner.delegations.is_empty());
    let retained = inner.engram_work_naming_history[0].transition.as_ref().unwrap();
    assert_eq!((&retained.id, retained.version), (&owner.id, owner.version));
    assert_eq!(retained.phase, EngramAuthorityPhase::Candidate);
}

#[test]
fn superseded_or_deadline_exhausted_candidate_recovery_never_admits() {
    for supersede in [true, false] {
        let fixture = Maintenance::new();
        fixture.strand_candidate();
        let state = fixture.state.clone();
        let clock = fixture.state.engram_budget_clock();
        let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let injected = observed.clone();
        fixture.state.inner.lock().unwrap().test_engram_authority_ack_boundary = Some(Arc::new(move |phase| {
            if phase == "before_recovery_publication" {
                injected.store(true, std::sync::atomic::Ordering::SeqCst);
                if supersede {
                    state.inner.lock().unwrap().engram_work_naming_history[0].transition.as_mut().unwrap().version += 1;
                } else { clock.advance(acceptance_evaluation_request_tracker_budget()); }
            }
        }));
        let result = fixture.request();
        fixture.state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;
        assert!(result.is_err());
        assert!(observed.load(std::sync::atomic::Ordering::SeqCst), "request attempted exact recovery");
        let inner = fixture.state.inner.lock().unwrap();
        assert!(inner.delegations.is_empty());
        assert_eq!(inner.engram_work_naming_history[0].transition.as_ref().unwrap().phase, EngramAuthorityPhase::Candidate);
    }
}

/// What an attempted automatic pass that keeps this fixture's root reports:
/// the fixture has no eligible reader, and that check comes first.
const KEPT_ROOT_OUTCOME: &str = "state unknown: no eligible reader in the original store; entry retained";

#[test]
fn a_kept_roots_pass_outcome_and_cooldown_are_named_in_the_in_flight_refusal() {
    let fixture = Maintenance::new();
    fixture.prepare();
    let root = fixture.state.inner.lock().unwrap().engram_work_source_roots[0].clone();
    fixture.state.run_engram_root_reclamation_pass(vec![EngramRootReclamationTask {
        root: root.clone(), route: EngramRootReclamationRoute::Unfocused,
    }]);
    let now = fixture.state.engram_budget_clock().now();
    let (status, reclamation) = {
        let inner = fixture.state.inner.lock().unwrap();
        assert!(inner.engram_work_source_roots.contains(&root), "the pass kept the root");
        assert!(inner.engram_root_reclamation.cooldowns.iter().any(|(selection, _)| selection == &root));
        (engram_named_root_status_locked(&inner, now).iter()
            .map(engram_named_root_status_line).collect::<Vec<_>>().join(" | "),
            engram_named_root_reclamation_text(&inner, &root, now))
    };
    assert!(status.contains("last automatic pass attempt 0 s ago"), "a dated last attempt: {status}");
    let outcome = KEPT_ROOT_OUTCOME;
    assert!(status.contains(&format!("refused: {outcome} (HTTP 409)")),
        "the status view names the kept root's last attempt: {status}");
    let error = fixture.request().err().expect("Prepared is not force-published");
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE, "{}", error.message);
    assert!(error.message.starts_with(ACCEPTANCE_EVALUATION_AUTHORITY_IN_FLIGHT), "{}", error.message);
    assert!(error.message.contains(outcome), "the refusal names the same outcome: {}", error.message);
    assert!(error.message.contains(&reclamation),
        "and the root's reclamation status as the status view gives it ({reclamation}): {}", error.message);
    let hint = ENGRAM_ROOT_RECLAMATION_COOLDOWN + ACCEPTANCE_EVALUATION_IN_FLIGHT_RETRY_AFTER;
    assert!(error.message.contains(&format!("again in {} seconds", hint.as_secs())),
        "the hint waits out the cooldown, never only the pass budget: {}", error.message);
    let inner = fixture.state.inner.lock().unwrap();
    assert!(inner.delegations.is_empty());
    assert_eq!(inner.engram_work_naming_history[0].transition.as_ref().unwrap().phase, EngramAuthorityPhase::Prepared);
}

#[test]
fn a_foreign_prepared_owner_remains_a_final_conflict() {
    let fixture = Maintenance::new();
    let mut foreign = fixture.binding.clone();
    foreign.claim_id = "another-claim".to_owned();
    foreign.run_id = "another-run".to_owned();
    fixture.state.prepare_engram_authority(&fixture.store, &foreign, Duration::from_secs(2)).unwrap();
    let error = fixture.request().err().expect("foreign owner cannot authorize this request");
    assert_eq!(error.status, StatusCode::CONFLICT, "{}", error.message);
    assert!(fixture.state.inner.lock().unwrap().delegations.is_empty());
}

#[test]
fn requested_claim_run_and_generation_drift_are_final_conflicts_despite_an_in_flight_binding() {
    for drift in ["claim", "run", "generation"] {
        let fixture = Maintenance::new();
        fixture.prepare();
        {
            let inner = fixture.state.inner.lock().unwrap();
            let history = &inner.engram_work_naming_history[0];
            let bound = EngramWorkNamingBound::of_entry(&inner.engram_work_source_roots[0], history).unwrap();
            assert!(engram_work_authority_transition_in_flight(&inner, &fixture.store,
                &fixture.binding.work_id, Some(&bound)), "{drift}: the existing predicate alone permits InFlight");
        }
        let mut core = evidence_selection::canonical_core_receipt();
        let mut claim_id = fixture.binding.claim_id.clone();
        match drift {
            "claim" => claim_id = "another-requested-claim".to_owned(),
            "run" => {
                let other_run = "01a0b6c5-4b4f-7b41-9e32-edd7dea8b370";
                core["status"]["work"]["active_run_id"] = json!(other_run);
                core["run"]["run_id"] = json!(other_run);
            }
            "generation" => core["run"]["generation"] = json!(2),
            _ => unreachable!(),
        }
        let error = fixture.request_with(core, &claim_id).err().expect("drift is a final conflict");
        assert_eq!(error.status, StatusCode::CONFLICT, "{drift}: {}", error.message);
        assert!(error.message.contains("a naming of it is still being settled"), "{drift}: {}", error.message);
        let inner = fixture.state.inner.lock().unwrap();
        assert!(inner.delegations.is_empty(), "{drift}");
        assert_eq!(inner.engram_work_naming_history[0].transition.as_ref().unwrap().phase,
            EngramAuthorityPhase::Prepared, "{drift}");
    }
}
