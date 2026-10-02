// Connected writer and canonical-history regressions for the per-work
// authority publication boundary. Reuses production naming fixtures.
use super::*;

mod budget {
    include!("engram_budget_authority.rs");
}

struct FailingAdmissionRootReader {
    control: Arc<ScriptedEngramControlTransport>,
    fail_after: &'static str,
    binding: EngramControlWorkBinding,
}

struct InterferingAdmissionRootReader {
    control: Arc<ScriptedEngramControlTransport>,
    state: AppState,
    store: EngramAuthorityStoreKey,
    binding: EngramControlWorkBinding,
    mode: &'static str,
    begun: std::sync::atomic::AtomicBool,
    interfered: std::sync::atomic::AtomicBool,
}

impl EngramControlTransport for InterferingAdmissionRootReader {
    fn shutdown_session(&self, session: &str) {
        self.control.shutdown_session(session);
    }
    fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.control
            .read_work_binding(connection, preference, timeout)
    }
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        let result = self.control.request(connection, request, timeout);
        if matches!(request, EngramControlRequest::TurnBegin { .. }) && result.is_ok() {
            self.begun.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        if matches!(request, EngramControlRequest::NamedRootRead { .. })
            && self.begun.load(std::sync::atomic::Ordering::SeqCst)
            && !self
                .interfered
                .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            match self.mode {
                "credentials" => {
                    return Err(EngramTransportError::remote(EngramControlErrorBody {
                        code: "control_session_token_mismatch".to_owned(),
                        message: "auxiliary source read unavailable".to_owned(),
                    }));
                }
                "unsupported" => {
                    return Err(EngramTransportError::remote(EngramControlErrorBody {
                        code: "unknown_control_operation".to_owned(),
                        message: "source reader unsupported".to_owned(),
                    }));
                }
                "malformed" => return Ok(json!({"result": {"unexpected": true}})),
                "store" => change_engram_settings(&self.state, |settings| {
                    settings
                        .authority_store_key
                        .as_mut()
                        .unwrap()
                        .project_id
                        .push_str("-successor");
                }),
                "connection" => change_engram_settings(&self.state, |settings| {
                    settings.binary_path = Some("successor-engram.exe".to_owned());
                }),
                _ => {}
            }
            if self.mode == "root-owner" {
                let proof = parse_engram_result::<EngramNamedRootReadResponse>(
                    result.as_ref().unwrap().clone(),
                )
                .unwrap();
                let owner = self
                    .state
                    .prepare_engram_authority(&self.store, &self.binding, DEADLOCK_GUARD)
                    .unwrap();
                AppState::learn_engram_authority_locked(
                    &mut self.state.inner.lock().unwrap(),
                    &self.store,
                    &owner,
                    &proof,
                )
                .unwrap();
                self.state
                    .publish_engram_authority(&self.store, &owner, DEADLOCK_GUARD)
                    .unwrap();
            } else {
                let mut inner = self.state.inner.lock().unwrap();
                let index = inner.find_session_index(&connection.session_id).unwrap();
                let record = &mut inner.sessions[index];
                record.engram.named_root = Some(EngramNamedRootState::Bound {
                    workspace_id: "successor-source".to_owned(),
                    generation: 99,
                    named_at: "2026-10-02T00:00:00Z".to_owned(),
                });
                match self.mode {
                    "routing" => record.engram.routing_token = Some("successor-token".to_owned()),
                    "claim" => record
                        .engram
                        .work_binding
                        .as_mut()
                        .unwrap()
                        .claim_id
                        .push_str("-successor"),
                    "queue" => record.engram.dispatch_generation += 1,
                    "turn" => record.active_turn_generation += 1,
                    "runtime" => record.runtime = SessionRuntime::None,
                    "store" | "connection" => {}
                    _ => unreachable!(),
                }
            }
        }
        result
    }
}

#[test]
fn source_root_authority_admission_separates_root_owner_from_routing_claim_and_queue_ownership() {
    for mode in [
        "root-owner",
        "routing",
        "claim",
        "queue",
        "turn",
        "runtime",
        "store",
        "connection",
        "credentials",
        "unsupported",
        "malformed",
    ] {
        let label = format!("admission-owner-{mode}");
        let grant = format!("{label}-grant");
        let claimed = ClaimedRoot::new_scripted(
            &label,
            vec![
                bind_reply("owner-token"),
                grant_reply(&grant),
                begin_reply(&grant),
                checkpoint_reply(&grant),
            ],
        );
        prepare_confirmed_claimed_opening(&claimed, &label);
        let store = claimed_root_store(&claimed);
        let binding = test_control_work_binding(&format!("turn-observation-{label}"), 1);
        let reader = Arc::new(InterferingAdmissionRootReader {
            control: claimed.transport.clone(),
            state: claimed.state.clone(),
            store: store.clone(),
            binding: binding.clone(),
            mode,
            begun: std::sync::atomic::AtomicBool::new(false),
            interfered: std::sync::atomic::AtomicBool::new(false),
        });
        install_control_only_transport(&claimed.state, reader.clone());
        let outcome = deliver_turn_dispatch(&claimed.state, claimed.dispatch());
        install_control_only_transport(&claimed.state, claimed.transport.clone());
        assert!(reader.interfered.load(std::sync::atomic::Ordering::SeqCst));
        let prompt = claimed.runtime_rx.try_recv();
        if matches!(mode, "root-owner" | "unsupported" | "malformed") {
            assert!(
                matches!(outcome, TurnDispatchDeliveryOutcome::Delivered) && prompt.is_ok(),
                "root-only owner supersession cannot revoke valid admission: {outcome:?}"
            );
            assert_eq!(
                claimed.state.engram_turn_root_capture(&claimed.session_id),
                EngramRootCapture::Unconfirmed
            );
            let restored = load_state(claimed.state.persistence_path.as_path())
                .unwrap()
                .unwrap();
            assert!(
                engram_authority_work_unresolved(&restored, &store, &binding.work_id),
                "expired proof must be replaced with a real acknowledged guard"
            );
            let observation = finish_claimed_turn(&claimed, &claimed.runtime_token());
            assert!(
                observation.get("source_basis").is_none(),
                "{mode}: withheld evidence cannot gain source credit at close"
            );
            assert_eq!(
                claimed
                    .transport
                    .requests()
                    .iter()
                    .filter(|request| request.request["operation"] == "turn_begin")
                    .count(),
                1
            );
        } else {
            assert!(
                !matches!(outcome, TurnDispatchDeliveryOutcome::Delivered) && prompt.is_err(),
                "{mode}: invalid admission cannot dispatch: {outcome:?}"
            );
            assert!(
                mode == "credentials"
                    || matches!(claimed.record(|r| r.engram.named_root.clone()), Some(EngramNamedRootState::Bound { workspace_id, .. }) if workspace_id == "successor-source"),
                "{mode}: stale source reply must not overwrite successor state"
            );
        }
    }
}

impl EngramControlTransport for FailingAdmissionRootReader {
    fn shutdown_session(&self, session_id: &str) {
        self.control.shutdown_session(session_id);
    }
    fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.control
            .read_work_binding(connection, preference, timeout)
    }

    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        if self.fail_after == "recover-on-begin"
            && matches!(request, EngramControlRequest::TurnBegin { .. })
        {
            self.control
                .named_roots
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .root_read_failures
                .remove(&self.binding.claim_id);
        }
        let mut result = self.control.request(connection, request, timeout);
        if self.fail_after == "unknown-begin"
            && result.is_ok()
            && matches!(request, EngramControlRequest::TurnBegin { .. })
        {
            let value = result.as_mut().unwrap();
            let value = if value.get("result").is_some() {
                value.get_mut("result").unwrap()
            } else {
                value
            };
            value["receipt"]["named_root"] = json!({"state": "unknown"});
        }
        if result.is_ok()
            && (serde_json::to_value(request).unwrap()["operation"] == self.fail_after
                || (self.fail_after == "recover-on-begin"
                    && matches!(request, EngramControlRequest::SessionBind { .. })))
        {
            self.control
                .named_roots
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .root_read_failures
                .insert(self.binding.claim_id.clone());
        }
        result
    }
}

#[test]
fn source_root_authority_admission_withholds_auxiliary_read_failures_and_legacy_history() {
    for mode in [
        "bind-read",
        "begin-read",
        "legacy",
        "recovered-bind",
        "unknown-begin",
    ] {
        let label = format!("admission-withheld-{mode}");
        let grant = format!("{label}-grant");
        let claimed = ClaimedRoot::new_scripted(
            &label,
            vec![
                bind_reply("withheld-token"),
                grant_reply(&grant),
                begin_reply(&grant),
                checkpoint_reply(&grant),
            ],
        );
        prepare_confirmed_claimed_opening(&claimed, &label);
        let store = claimed_root_store(&claimed);
        let binding = test_control_work_binding(&format!("turn-observation-{label}"), 1);
        if mode == "legacy" {
            engram_record_naming_history(
                &mut claimed.state.inner.lock().unwrap(),
                &store,
                &binding.work_id,
                8,
            );
        } else {
            install_control_only_transport(
                &claimed.state,
                Arc::new(FailingAdmissionRootReader {
                    control: claimed.transport.clone(),
                    binding: binding.clone(),
                    fail_after: if mode == "bind-read" {
                        "session_bind"
                    } else if mode == "recovered-bind" {
                        "recover-on-begin"
                    } else if mode == "unknown-begin" {
                        "unknown-begin"
                    } else {
                        "turn_begin"
                    },
                }),
            );
        }
        let delivery = deliver_turn_dispatch(&claimed.state, claimed.dispatch());
        let prompt = claimed.runtime_rx.try_recv();
        assert!(
            matches!(delivery, TurnDispatchDeliveryOutcome::Delivered) && prompt.is_ok(),
            "{mode}: definitive admission must reach the runtime with source evidence withheld: delivery={delivery:?}, prompt_received={}",
            prompt.is_ok()
        );
        let CodexRuntimeCommand::Prompt { command, .. } = prompt.unwrap() else {
            panic!("expected prompt");
        };
        if mode == "recovered-bind" {
            assert!(
                !command.prompt.contains("unconfirmed"),
                "{}",
                command.prompt
            );
            assert!(claimed.record(|r| r.engram.active_turn_start_basis.is_some()));
            let observation = finish_claimed_turn(&claimed, &claimed.runtime_token());
            assert!(observation.get("source_basis").is_some(), "{observation}");
            continue;
        }
        assert!(
            !command.prompt.contains("lacks opening provenance"),
            "{}",
            command.prompt
        );
        let cause = if mode == "legacy" {
            "canonical naming history"
        } else if mode == "unknown-begin" {
            "unknown opening authority"
        } else {
            "canonical authority read failed"
        };
        assert!(command.prompt.contains(cause), "{}", command.prompt);
        assert_eq!(
            command
                .prompt
                .matches("This turn's source-root binding is unconfirmed")
                .count(),
            1
        );
        assert!(
            command.prompt.contains("unconfirmed"),
            "{mode}: actionable source uncertainty notice"
        );
        assert_eq!(
            claimed.state.engram_turn_root_capture(&claimed.session_id),
            EngramRootCapture::Unconfirmed
        );
        assert!(claimed.record(|r| r.engram.active_turn_start_basis.is_none()));
        let restored = load_state(claimed.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        assert!(
            engram_authority_work_unresolved(&restored, &store, &binding.work_id),
            "{mode}: an acknowledged durable record must retain restart withholding"
        );
        let observation = finish_claimed_turn(&claimed, &claimed.runtime_token());
        assert!(
            observation.get("source_basis").is_none(),
            "{mode}: no source credit"
        );
        let checkpoints = claimed
            .transport
            .requests()
            .into_iter()
            .filter(|r| r.request["operation"] == "turn_checkpoint")
            .collect::<Vec<_>>();
        assert_eq!(
            checkpoints.len(),
            1,
            "{mode}: ordinary original-grant close"
        );
        assert_eq!(checkpoints[0].request["grant_id"], grant);
        assert_eq!(
            claimed
                .transport
                .requests()
                .iter()
                .filter(|r| r.request["operation"] == "turn_begin")
                .count(),
            1,
            "{mode}: uncertainty does not authorize replacement admission"
        );
    }
}

#[test]
fn source_root_authority_admission_distinguishes_candidate_uncertainty_from_missing_durability() {
    for mode in ["candidate", "guard", "admission"] {
        let label = format!("admission-writer-{mode}");
        let grant = format!("{label}-grant");
        let mut claimed = ClaimedRoot::new(
            &label,
            vec![
                bind_reply("writer-token"),
                grant_reply(&grant),
                begin_reply(&grant),
                checkpoint_reply(&grant),
            ],
        );
        prepare_confirmed_claimed_opening(&claimed, &label);
        let store = claimed_root_store(&claimed);
        let binding = test_control_work_binding(&format!("turn-observation-{label}"), 1);
        let (tx, rx) = std::sync::mpsc::channel();
        claimed.state.persist_tx = tx;
        let worker = claimed.state.clone();
        let session = claimed.session_id.clone();
        let task = std::thread::spawn(move || {
            let dispatch = match worker
                .dispatch_turn(
                    &session,
                    SendMessageRequest {
                        text: "Continue while source evidence is withheld.".to_owned(),
                        expanded_text: None,
                        attachments: Vec::new(),
                        source_session_id: None,
                        source_mailbox: None,
                    },
                )
                .unwrap()
            {
                DispatchTurnResult::Dispatched(dispatch)
                | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
                DispatchTurnResult::Queued => panic!("idle fixture must dispatch"),
            };
            let outcome = deliver_turn_dispatch(&worker, dispatch);
            if mode == "candidate" && matches!(outcome, TurnDispatchDeliveryOutcome::Delivered) {
                let runtime = {
                    let inner = worker.inner.lock().unwrap();
                    inner.sessions[inner.find_session_index(&session).unwrap()]
                        .runtime
                        .runtime_token()
                        .unwrap()
                };
                worker
                    .finish_turn_ok_if_runtime_matches(&session, &runtime)
                    .unwrap();
            }
            outcome
        });
        let mut batch = PersistFenceBatch::default();
        let mut cache = SqlitePersistConnectionCache::new();
        let mut candidates = 0;
        let mut failed = false;
        while !task.is_finished() {
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(PersistRequest::Fence(fence)) => {
                    let inject = match &fence.target {
                        PersistFenceTarget::EngramWorkAuthority(image) => {
                            let phase = image.history.transition.as_ref().unwrap().phase;
                            if phase == EngramAuthorityPhase::Candidate {
                                candidates += 1;
                            }
                            (mode == "guard" && phase == EngramAuthorityPhase::Prepared)
                                || (mode == "candidate"
                                    && candidates == 2
                                    && phase == EngramAuthorityPhase::Candidate)
                        }
                        PersistFenceTarget::EngramAdmission { content, .. } => {
                            mode == "admission" && content["grant"] == grant
                        }
                        _ => false,
                    };
                    if inject && !failed {
                        failed = true;
                        fence.finish(Err(PersistFenceError::WriteFailed(
                            "controlled boundary failure".to_owned(),
                        )));
                    } else {
                        batch.accept(PersistRequest::Fence(fence));
                        let delta =
                            collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
                        persist_delta_with_fences(
                            &mut cache,
                            claimed.state.persistence_path.as_path(),
                            &delta,
                            &mut batch,
                        )
                        .unwrap();
                    }
                }
                Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(error) => panic!("writer fixture disconnected: {error}"),
            }
        }
        let outcome = task.join().unwrap();
        assert!(
            failed,
            "{mode}: intended durability boundary must be reached"
        );
        let prompt = claimed.runtime_rx.try_recv();
        if mode == "candidate" {
            assert!(
                matches!(outcome, TurnDispatchDeliveryOutcome::Delivered) && prompt.is_ok(),
                "{mode}: acknowledged guard plus independent admission fence permit delivery: {outcome:?}"
            );
            assert_eq!(
                claimed.state.engram_turn_root_capture(&claimed.session_id),
                EngramRootCapture::Unconfirmed
            );
            let restored = load_state(claimed.state.persistence_path.as_path())
                .unwrap()
                .unwrap();
            assert!(engram_authority_work_unresolved(
                &restored,
                &store,
                &binding.work_id
            ));
            let checkpoints = claimed
                .transport
                .requests()
                .into_iter()
                .filter(|r| r.request["operation"] == "turn_checkpoint")
                .collect::<Vec<_>>();
            assert_eq!(checkpoints.len(), 1);
            assert_eq!(checkpoints[0].request["grant_id"], grant);
            assert!(
                checkpoints[0].request["observations"][0]
                    .get("source_basis")
                    .is_none()
            );
        } else {
            assert!(
                !matches!(outcome, TurnDispatchDeliveryOutcome::Delivered) && prompt.is_err(),
                "{mode}: missing required durability cannot become a notice-only success: {outcome:?}"
            );
            if mode == "guard" {
                assert!(!claimed.transport.requests().iter().any(|r| matches!(
                    r.request["operation"].as_str(),
                    Some("session_bind" | "turn_begin" | "named_root_read")
                )));
            }
        }
        assert!(
            claimed
                .transport
                .requests()
                .iter()
                .filter(|r| r.request["operation"] == "turn_begin")
                .count()
                <= 1,
            "{mode}: failure cannot repeat or replace admission"
        );
    }
}

#[test]
fn source_root_authority_unclaimed_admission_needs_no_claimed_recovery_guard() {
    unclaimed_opening_keeps_claimed_diagnostics_absent(false);
}

#[test]
fn source_root_authority_unclaimed_unknown_needs_no_claimed_opening_diagnostic() {
    unclaimed_opening_keeps_claimed_diagnostics_absent(true);
}

fn unclaimed_opening_keeps_claimed_diagnostics_absent(unknown: bool) {
    let label = "admission-no-claim";
    let grant = "admission-no-claim-grant";
    let fresh_grant = "admission-no-claim-fresh-grant";
    let bind = if unknown {
        ScriptedEngramControlResponse::Reply(Ok(
            json!({"routing_token":"no-claim-token", "status":{"phase":"ready", "named_root":{"state":"future_variant"}}}),
        ))
    } else {
        bind_reply("no-claim-token")
    };
    let begin = if unknown {
        ScriptedEngramControlResponse::Reply(Ok(
            json!({"decision":"begin", "receipt":{"grant_id":grant, "named_root":{"state":"future_variant"}}}),
        ))
    } else {
        begin_reply(grant)
    };
    let claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind,
            grant_reply(grant),
            begin,
            checkpoint_reply(grant),
            grant_reply(fresh_grant),
            begin_reply(fresh_grant),
            checkpoint_reply(fresh_grant),
        ],
    );
    claimed_root_store(&claimed);
    *claimed.transport.work_bindings.lock().unwrap() = [Ok(None)].into_iter().collect();
    assert!(matches!(
        deliver_turn_dispatch(&claimed.state, claimed.dispatch()),
        TurnDispatchDeliveryOutcome::Delivered
    ));
    let prompt = received_prompt(&claimed);
    assert!(!prompt.contains("unconfirmed"), "{prompt}");
    // No work is associated with this grant. Wire omission is normal here;
    // it creates neither a claimed opening capture nor a naming obligation.
    assert_eq!(
        claimed.state.engram_root_capture(&claimed.session_id),
        EngramRootCapture::Unnamed
    );
    claimed.record(|record| {
        assert!(record.engram.work_binding.is_none());
        assert!(record.engram.active_turn_root_capture.is_none());
        assert!(record.engram.active_turn_start_basis.is_none());
        assert!(record.engram.pending_source_root_line.is_none());
        assert!(record.engram.opening_diagnostic.is_none());
    });
    let restored = load_state(claimed.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(restored.engram_work_naming_history.is_empty());
    assert!(restored.engram_named_root_journal.is_empty());
    assert!(claimed.record(|record| record.engram.work_binding.is_none()));
    claimed
        .state
        .finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token())
        .unwrap();
    let requests = claimed.transport.requests();
    assert!(
        !requests
            .iter()
            .any(|request| request.request["operation"] == "named_root_read")
    );
    let checkpoint = requests
        .iter()
        .find(|request| request.request["operation"] == "turn_checkpoint")
        .unwrap();
    assert!(
        checkpoint
            .request
            .get("observations")
            .is_none_or(|observations| observations.as_array().is_some_and(Vec::is_empty)),
        "unbound grant closes observationlessly: {}",
        checkpoint.request
    );
    assert!(checkpoint.request.get("verification_evidence").is_none());
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.request["operation"] == "turn_begin")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.request["operation"] == "turn_checkpoint"
                && request.request["grant_id"] == grant)
            .count(),
        1
    );
    claimed
        .transport
        .work_bindings
        .lock()
        .unwrap()
        .push_back(Ok(None));
    deliver_turn_dispatch(&claimed.state, claimed.dispatch()).unwrap();
    let prompt = received_prompt(&claimed);
    assert!(
        !prompt.contains("unconfirmed"),
        "an unclaimed successor inherits no opening diagnostic: {prompt}"
    );
    claimed
        .state
        .finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token())
        .unwrap();
}

#[test]
fn source_root_authority_invalid_held_association_cannot_poison_a_later_name() {
    for case in ["missing", "work", "claim", "fence", "run", "execution"] {
        let label = format!("invalid-association-{case}");
        let claimed = ClaimedRoot::new_scripted(&label, Vec::new());
        let worktree = add_claimed_root_worktree(&claimed.root);
        prepare_claimed_root_naming(&claimed, &label);
        claimed_root_store(&claimed);
        let before = claimed
            .state
            .inner
            .lock()
            .unwrap()
            .engram_source_root_generation;
        let mut invalid = claimed_root_held(&label);
        match case {
            "missing" => invalid.items[0].control_binding = None,
            "work" => invalid.items[0]
                .control_binding
                .as_mut()
                .unwrap()
                .work_id
                .push_str("-other"),
            "claim" => invalid.items[0]
                .control_binding
                .as_mut()
                .unwrap()
                .claim_id
                .push_str("-other"),
            "fence" => {
                invalid.items[0]
                    .control_binding
                    .as_mut()
                    .unwrap()
                    .claim_fence += 1
            }
            "run" => invalid.items[0]
                .control_binding
                .as_mut()
                .unwrap()
                .run_id
                .clear(),
            "execution" => invalid.items[0]
                .control_binding
                .as_mut()
                .unwrap()
                .root_execution_id
                .clear(),
            _ => unreachable!(),
        }
        let rejected = name_root(&claimed, &label, Some(&worktree), vec![invalid]);
        let (generation, journals) = {
            let inner = claimed.state.inner.lock().unwrap();
            (
                inner.engram_source_root_generation,
                inner.engram_named_root_journal.clone(),
            )
        };
        assert!(
            rejected.is_err(),
            "{case}: invalid held association must refuse"
        );
        assert_eq!(
            generation, before,
            "{case}: refusal must not allocate a generation"
        );
        assert!(
            journals.is_empty(),
            "{case}: refusal must not retain an unsendable intent: {journals:?}"
        );
        let valid = name_root(
            &claimed,
            &label,
            Some(&worktree),
            vec![claimed_root_held(&label)],
        )
        .expect("a rejected fresh request cannot poison a later valid name");
        assert_eq!(valid.generation, before + 1);
    }
}

#[test]
fn source_root_authority_shared_staging_and_cleanup_require_retained_association() {
    let label = "shared-intent-association";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    let mut inner = claimed.state.inner.lock().unwrap();
    let original = inner.engram_named_root_journal[0].clone();
    let mut event = original.confirmed.as_ref().unwrap().0.clone();
    event.root.work_id.push_str("-fresh");
    event.root.claim_id.push_str("-fresh");
    let before = inner.engram_named_root_journal.clone();
    assert!(
        claimed
            .state
            .stage_engram_root_event_locked(&mut inner, &event, None)
            .is_err()
    );
    assert_eq!(
        inner.engram_named_root_journal, before,
        "shared staging refuses before any mutation"
    );
    let mut pending = original.confirmed.as_ref().unwrap().0.clone();
    pending.root.generation += 1;
    inner.engram_named_root_journal[0].pending = Some(pending.clone());
    inner.engram_named_root_journal[0].read_binding = None;
    assert!(
        claimed
            .state
            .stage_engram_root_event_locked(&mut inner, &pending, None)
            .is_err()
    );
    assert_eq!(
        inner.engram_named_root_journal[0].pending.as_ref(),
        Some(&pending),
        "uncertain legacy send bytes cannot be erased on association refusal"
    );
    inner.engram_named_root_journal[0] = original.clone();
    inner.engram_named_root_journal[0].read_binding = None;
    let root = original.confirmed.as_ref().unwrap().0.root.clone();
    engram_queue_root_cleanup(
        &mut inner.engram_named_root_journal,
        &root,
        EngramNamedRootEndReason::RootInvalid,
    );
    assert!(
        inner.engram_named_root_journal[0].pending.is_none(),
        "cleanup cannot invent an unsendable Ended intent"
    );
    assert!(inner.engram_named_root_journal[0].requires_reconciliation());
    assert_eq!(
        inner.engram_named_root_journal[0].confirmed,
        original.confirmed
    );
    assert_eq!(
        engram_root_capture_locked(&inner, &claimed.session_id),
        EngramRootCapture::Unconfirmed
    );
    drop(inner);
    let delta = collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
    let mut cache = SqlitePersistConnectionCache::new();
    persist_delta_via_cache(&mut cache, claimed.state.persistence_path.as_path(), &delta).unwrap();
    let restored = load_state(claimed.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(
        engram_authority_work_unresolved(&restored, &root.store, &root.work_id),
        "the association recovery guard must survive restoration"
    );
    assert_eq!(
        restored.engram_named_root_journal[0].confirmed,
        original.confirmed
    );
}

#[test]
fn source_root_authority_fixture_setup_ack_deadline_distinguishes_expiry_from_owner_change() {
    for mode in ["standalone-expiry", "setup-success", "setup-expiry"] {
        let label = format!("authority-{mode}");
        let claimed = ClaimedRoot::new(&label, vec![bind_reply("setup-token")]);
        prepare_confirmed_claimed_opening(&claimed, &label);
        let store = claimed_root_store(&claimed);
        let recorder = Arc::new(AuthorityTimeoutRecorder {
            control: claimed.transport.clone(),
            calls: Mutex::new(Vec::new()),
        });
        install_control_only_transport(&claimed.state, recorder.clone());
        let target = AppState::engram_binding_target_for_session_shape_locked(
            &claimed.state.inner.lock().unwrap(),
            &claimed.session_id,
            true,
        )
        .unwrap()
        .unwrap();
        let rpc = target.settings.call_timeout();
        assert_eq!(rpc, Duration::from_millis(250));
        assert!(target.admission_started_at.is_none());
        let mut state = claimed.state.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        state.persist_tx = tx;
        let worker = state.clone();
        let success = mode == "setup-success";
        let task = std::thread::spawn(move || {
            if mode == "standalone-expiry" {
                worker.bind_engram_target_off_lock(target)
            } else {
                let total = if success { DEADLOCK_GUARD } else { rpc };
                worker.bind_engram_fixture_setup_off_lock(target, std::time::Instant::now() + total)
            }
        });
        let mut batch = PersistFenceBatch::default();
        let mut cache = SqlitePersistConnectionCache::new();
        receive_naming_authority_fence(&rx, &mut batch, &task);
        let before = state.inner.lock().unwrap().engram_work_naming_history[0]
            .transition
            .clone()
            .unwrap();
        let mut expected_owner = before.clone();
        assert_eq!(before.phase, EngramAuthorityPhase::Prepared);
        assert!(
            state.inner.inner.try_lock().is_ok(),
            "ACK wait stays off StateInner"
        );
        if success {
            // The correct first ACK arrives after the unchanged RPC interval,
            // while the single explicit setup total is still funded.
            std::thread::sleep(rpc + Duration::from_millis(25));
            let guard = collect_persist_delta_from_shared_state(&state.inner, 0);
            persist_delta_with_fences(
                &mut cache,
                state.persistence_path.as_path(),
                &guard,
                &mut batch,
            )
            .unwrap();
            receive_naming_authority_fence(&rx, &mut batch, &task);
            expected_owner = state.inner.lock().unwrap().engram_work_naming_history[0]
                .transition
                .clone()
                .unwrap();
            assert_eq!(expected_owner.phase, EngramAuthorityPhase::Candidate);
            assert_eq!(
                expected_owner.id, before.id,
                "post-bind reuses the acknowledged pre-RPC owner"
            );
            assert_eq!(expected_owner.version, before.version);
            assert_eq!(expected_owner.binding, before.binding);
            let current = state.inner.lock().unwrap().engram_work_naming_history[0]
                .transition
                .clone()
                .unwrap();
            assert_eq!(current.id, expected_owner.id);
            assert_eq!(current.version, expected_owner.version);
            assert_eq!(current.phase, EngramAuthorityPhase::Candidate);
            let candidate = collect_persist_delta_from_shared_state(&state.inner, 0);
            persist_delta_with_fences(
                &mut cache,
                state.persistence_path.as_path(),
                &candidate,
                &mut batch,
            )
            .unwrap();
            assert_eq!(task.join().unwrap().unwrap(), "setup-token");
        } else {
            // Withhold the exact Prepared ACK until the total expires. This
            // distinguishes deadline failure from supersession or wrong content.
            let error = task
                .join()
                .unwrap()
                .expect_err("no ACK before this operation's deadline");
            assert!(error.message.contains("(Deadline)"), "{mode}: {error:?}");
            assert!(
                !recorder
                    .calls
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(op, _)| op == "named_root_read")
            );
            assert!(
                !rx.try_iter()
                    .any(|request| matches!(request, PersistRequest::Fence(_)))
            );
            let late = collect_persist_delta_from_shared_state(&state.inner, 0);
            persist_delta_with_fences(
                &mut cache,
                state.persistence_path.as_path(),
                &late,
                &mut batch,
            )
            .unwrap();
        }
        let inner = state.inner.lock().unwrap();
        let after = inner.engram_work_naming_history[0]
            .transition
            .as_ref()
            .unwrap();
        assert_eq!(
            after.id, expected_owner.id,
            "ACK belongs to the prepared owner"
        );
        assert_eq!(after.version, expected_owner.version);
        assert_eq!(after.binding, expected_owner.binding);
        assert_eq!(
            after.phase,
            if success {
                EngramAuthorityPhase::Published
            } else {
                EngramAuthorityPhase::Prepared
            }
        );
        assert_eq!(
            engram_authority_work_unresolved(&inner, &store, &before.binding.work_id),
            !success
        );
        let calls = recorder.calls.lock().unwrap();
        assert!(
            calls
                .iter()
                .filter(|(op, _)| mode != "standalone-expiry" || op != "work_binding_read")
                .all(|(_, timeout)| !timeout.is_zero() && *timeout <= rpc),
            "{mode}: {calls:?}"
        );
        assert_eq!(
            calls.iter().filter(|(op, _)| op == "session_bind").count(),
            usize::from(success)
        );
        assert_eq!(
            calls
                .iter()
                .filter(|(op, _)| op == "named_root_read")
                .count(),
            usize::from(success)
        );
        assert!(
            !calls
                .iter()
                .any(|(op, _)| op == "named_root_bind" || op == "turn_begin")
        );
    }
}

#[test]
fn source_root_authority_review_delayed_manual_writer_expires_without_owner_supersession() {
    let label = "authority-ack-contention-diagnostic";
    let claimed = ClaimedRoot::new(label, Vec::new());
    prepare_claimed_root_naming(&claimed, label);
    let store = claimed_root_store(&claimed);
    let (binding, token) = claimed.record(|record| {
        (
            record.engram.work_binding.clone().unwrap(),
            record.engram.routing_token.clone().unwrap(),
        )
    });
    let target = AppState::engram_binding_target_for_session_shape_locked(
        &claimed.state.inner.lock().unwrap(),
        &claimed.session_id,
        true,
    )
    .unwrap()
    .unwrap();
    let rpc_budget = target.settings.call_timeout();
    assert!(
        target.test_dispatch_budget.unwrap() > rpc_budget,
        "the established fixture declares control-ordering headroom separately from its RPC deadline"
    );
    let writer = sqlite_state_write_lock(claimed.state.persistence_path.as_path());
    let held = lock_sqlite_state_writer(&writer);
    let ticket = sqlite_state_writer_issued_tickets(&writer) + 1;
    let state = claimed.state.clone();
    let session = claimed.session_id.clone();
    let worker_binding = binding.clone();
    let task = std::thread::spawn(move || {
        let fence = EngramRootReadFence::capture(&state.inner.lock().unwrap());
        state.reconcile_engram_named_root_with_fence_within(
            &session,
            &token,
            Some(&worker_binding),
            Some(EngramNamedRootState::None),
            &fence,
            None,
            rpc_budget,
        )
    });
    wait_for_sqlite_state_writer_issued_tickets(&writer, ticket);
    let owner = claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_work_naming_history[0]
        .transition
        .clone()
        .unwrap();
    // Deliberately exceed the configured RPC-sized authority interval while
    // the actual SQLite writer is blocked. This is an ordered cause witness,
    // not a wall-time performance threshold or a retry-until-pass assertion.
    std::thread::sleep(rpc_budget + Duration::from_millis(25));
    drop(held);
    let error = task.join().unwrap().unwrap_err();
    assert!(
        error.message.contains("acknowledgement exceeded"),
        "{error:?}"
    );
    let inner = claimed.state.inner.lock().unwrap();
    assert_eq!(
        inner.engram_work_naming_history[0]
            .transition
            .as_ref()
            .unwrap(),
        &owner
    );
    assert!(engram_authority_work_unresolved(
        &inner,
        &store,
        &binding.work_id
    ));
    assert_eq!(
        engram_root_capture_locked(&inner, &claimed.session_id),
        EngramRootCapture::Unconfirmed
    );
    drop(inner);
    assert!(
        !claimed
            .transport
            .requests()
            .iter()
            .any(|request| request.request["operation"] == "named_root_read"
                || request.request["operation"] == "named_root_bind")
    );
}

#[test]
fn source_root_authority_review_restart_recovers_committed_candidate_without_binding_replay() {
    authority_review_restart_recovers(false);
}

#[test]
fn source_root_authority_review_restart_recovers_prepared_guard_without_binding_replay() {
    authority_review_restart_recovers(true);
}

fn authority_review_restart_recovers(prepared_only: bool) {
    let label = if prepared_only {
        "authority-restart-prepared"
    } else {
        "authority-restart-candidate"
    };
    let claimed = ClaimedRoot::new(label, Vec::new());
    prepare_claimed_root_naming(&claimed, label);
    let store = claimed_root_store(&claimed);
    let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    let old_token = engram_work_naming_token(
        &claimed.state.inner.lock().unwrap(),
        &store,
        &binding.work_id,
    );
    let tree = add_claimed_root_worktree(&claimed.root);
    name_root(&claimed, label, Some(&tree), vec![claimed_root_held(label)]).unwrap();
    let binds_before = claimed
        .transport
        .requests()
        .iter()
        .filter(|request| request.request["operation"] == "named_root_bind")
        .count();
    let mut state = claimed.state.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    state.persist_tx = tx;
    let worker = state.clone();
    let worker_store = store.clone();
    let worker_binding = binding.clone();
    let session = claimed.session_id.clone();
    let task = std::thread::spawn(move || {
        // This fixture drives a committed crash image, not expiry. Its
        // existing phase guard funds one operation across all three phases.
        let deadline = std::time::Instant::now() + TEST_PHASE_DEADLOCK_GUARD;
        let owner = worker
            .prepare_engram_authority_until(&worker_store, &worker_binding, deadline)
            .inspect_err(|error| eprintln!("restart preparation failed: {error:?}"))?;
        if prepared_only {
            return Ok::<_, ApiError>(());
        }
        let target = AppState::engram_binding_target_for_session_shape_locked(
            &worker.inner.lock().unwrap(),
            &session,
            true,
        )
        .unwrap()
        .unwrap();
        let proof = worker
            .read_engram_authority_fact_until(&target, &worker_binding, deadline)
            .inspect_err(|error| eprintln!("restart read failed: {error:?}"))?;
        AppState::learn_engram_authority_locked(
            &mut worker.inner.lock().unwrap(),
            &worker_store,
            &owner,
            &proof,
        )
        .inspect_err(|error| eprintln!("restart learn failed: {error:?}"))?;
        let result = worker.publish_engram_authority_until(&worker_store, &owner, deadline);
        eprintln!("restart candidate worker result: {result:?}");
        result
    });
    let mut batch = PersistFenceBatch::default();
    let mut cache = SqlitePersistConnectionCache::new();
    receive_naming_authority_fence(&rx, &mut batch, &task);
    let guarded = collect_persist_delta_from_shared_state(&state.inner, 0);
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &guarded,
        &mut batch,
    )
    .unwrap();
    if prepared_only {
        task.join().unwrap().unwrap();
    } else {
        receive_naming_authority_fence(&rx, &mut batch, &task);
        let candidate = collect_persist_delta_from_shared_state(&state.inner, guarded.watermark);
        persist_delta_via_cache(&mut cache, state.persistence_path.as_path(), &candidate).unwrap();
        batch.fail(PersistFenceError::WriteFailed(
            "committed but acknowledgement lost".to_owned(),
        ));
        let error = task.join().unwrap().unwrap_err();
        assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            error.message,
            "named-root authority persistence is unconfirmed (WriteFailed(\"committed but acknowledgement lost\")); recovery remains withheld",
            "the committed Candidate must fail through the injected lost acknowledgement"
        );
    }
    let restored = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(engram_authority_work_unresolved(
        &restored,
        &store,
        &binding.work_id
    ));
    assert_eq!(
        restored.engram_work_naming_history[0]
            .transition
            .as_ref()
            .unwrap()
            .phase,
        if prepared_only {
            EngramAuthorityPhase::Prepared
        } else {
            EngramAuthorityPhase::Candidate
        }
    );
    *claimed.state.inner.lock().unwrap() = restored;
    install_control_only_transport(&claimed.state, claimed.transport.clone());
    prepare_claimed_root_naming(&claimed, label);
    claimed
        .state
        .recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    let inner = claimed.state.inner.lock().unwrap();
    assert!(!engram_authority_work_unresolved(
        &inner,
        &store,
        &binding.work_id
    ));
    assert_eq!(
        inner.engram_work_naming_history[0]
            .transition
            .as_ref()
            .unwrap()
            .phase,
        EngramAuthorityPhase::Published
    );
    assert!(!engram_work_naming_is_current(&inner, &store, &old_token));
    assert_eq!(
        inner.engram_named_root_journal[0]
            .confirmed
            .as_ref()
            .unwrap()
            .1
            .generation,
        1
    );
    drop(inner);
    assert_eq!(
        claimed
            .transport
            .requests()
            .iter()
            .filter(|request| request.request["operation"] == "named_root_bind")
            .count(),
        binds_before,
        "restart recovery reads and acknowledges its image without sending another bind or fabricating a receipt"
    );
}

#[test]
fn source_root_authority_review_first_contact_none_recovers_remote_ended_history() {
    for case in ["ended", "completed", "no-event"] {
        authority_review_first_contact(case);
    }
}

#[test]
fn source_root_canonical_live_run_states_establish_first_contact_absence() {
    // Engram's run lifecycle is distinct from the work item's open lifecycle.
    // A current claim is claimed before admission and active after begin.
    for case in ["claimed-no-event", "active-no-event"] {
        authority_review_first_contact(case);
    }
}

#[test]
fn source_root_canonical_run_state_and_history_contract() {
    let claimed = ClaimedRoot::new("canonical-state-contract", Vec::new());
    prepare_claimed_root_naming(&claimed, "canonical-state-contract");
    let store = claimed_root_store(&claimed);
    let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    let base = json!({
        "project_id":store.project_id,"work_id":binding.work_id,
        "root_execution_id":binding.root_execution_id,"run_id":binding.run_id,
        "claim_id":binding.claim_id,"run":{"state":"claimed","generation":1},
        "named_root":{"state":"none"},"latest_event":null,
        "read_cut":{"feed":{"kind":"run_execution","id":binding.run_id},"position":4}
    });
    let valid = |value: Value| {
        serde_json::from_value::<EngramNamedRootReadResponse>(value)
            .unwrap()
            .validate_authority(&store, &binding)
            .is_ok()
    };
    let event = json!({"event":"canonical-bound-event",
        "position":{"feed":{"kind":"run_execution","id":binding.run_id},"position":2},
        "kind":"bound","generation":1,"workspace_id":"canonical-workspace",
        "named_at":"2026-10-01T00:00:00Z"});
    // Producer WorkRunState, not the work item's open lifecycle. A live run
    // may prove absence, but cannot hide a still-Bound event as terminal.
    for state in ["open", "claimed", "active", "completed", "cancelled"] {
        let mut proof = base.clone();
        proof["run"]["state"] = json!(state);
        assert!(valid(proof.clone()), "{state}: no-event absence");
        proof["latest_event"] = event.clone();
        assert_eq!(
            valid(proof.clone()),
            matches!(state, "completed" | "cancelled"),
            "{state}: absent projection with latest Bound"
        );
        proof["latest_event"]["kind"] = json!("ended");
        assert!(valid(proof.clone()), "{state}: explicit Ended absence");
        proof["latest_event"] = event.clone();
        proof["named_root"] = json!({"state":"bound","workspace_id":"canonical-workspace",
            "generation":1,"named_at":"2026-10-01T00:00:00Z"});
        assert!(valid(proof.clone()), "{state}: matching Bound");
        proof["named_root"] = json!({"state":"unbound_by_release",
            "last_generation":1,"released_at_position":3});
        assert!(valid(proof.clone()), "{state}: matching release");
        proof["named_root"]["last_generation"] = json!(2);
        assert!(!valid(proof), "{state}: contradictory release");
    }
    for state in ["", "ready", "future_run_state", "Claimed"] {
        let mut proof = base.clone();
        proof["run"]["state"] = json!(state);
        assert!(!valid(proof), "unknown run state {state:?}");
    }
    for (path, replacement) in [
        (vec!["project_id"], json!("other-project")),
        (vec!["work_id"], json!("other-work")),
        (vec!["run_id"], json!("other-run")),
        (vec!["claim_id"], json!("other-claim")),
        (vec!["root_execution_id"], json!("other-execution")),
        (vec!["run", "generation"], json!(0)),
        (vec!["read_cut", "feed", "id"], json!("other-run")),
        (vec!["read_cut", "feed", "kind"], json!("other-feed")),
        (vec!["read_cut", "position"], json!(-1)),
    ] {
        let mut proof = base.clone();
        let mut target = &mut proof;
        for field in &path {
            target = &mut target[*field];
        }
        *target = replacement;
        assert!(!valid(proof), "contradictory association {path:?}");
    }
    for (path, replacement) in [
        (vec!["position", "position"], json!(5)),
        (vec!["position", "feed", "id"], json!("other-run")),
        (vec!["generation"], json!(0)),
        (vec!["event"], json!("")),
        (vec!["workspace_id"], json!("")),
        (vec!["named_at"], json!("invalid")),
    ] {
        let mut proof = base.clone();
        proof["latest_event"] = event.clone();
        proof["latest_event"]["kind"] = json!("ended");
        let mut target = &mut proof["latest_event"];
        for field in &path {
            target = &mut target[*field];
        }
        *target = replacement;
        assert!(!valid(proof), "malformed event {path:?}");
    }
}

fn authority_review_first_contact(case: &str) {
    let label = "authority-first-contact";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    prepare_claimed_root_naming(&claimed, label);
    let store = claimed_root_store(&claimed);
    let (binding, token) = claimed.record(|record| {
        (
            record.engram.work_binding.clone().unwrap(),
            record.engram.routing_token.clone().unwrap(),
        )
    });
    let before = engram_work_naming_token(
        &claimed.state.inner.lock().unwrap(),
        &store,
        &binding.work_id,
    );
    let mut proof = json!({
        "project_id":store.project_id,"work_id":binding.work_id,
        "root_execution_id":binding.root_execution_id,"run_id":binding.run_id,
        "claim_id":binding.claim_id,"run":{"state":"open","generation":1},
        "named_root":{"state":"none"},
        "latest_event":{"event":"foreign-ended-event",
            "position":{"feed":{"kind":"run_execution","id":binding.run_id},"position":2},
            "kind":"ended","generation":1,
            "workspace_id":claimed.root.to_string_lossy(),
            "named_at":"2026-10-01T00:00:00Z"},
        "read_cut":{"feed":{"kind":"run_execution","id":binding.run_id},"position":2}
    });
    if case == "completed" {
        proof["run"]["state"] = json!("completed");
        proof["latest_event"]["kind"] = json!("bound");
    } else if case.ends_with("no-event") {
        proof["run"]["state"] = json!(match case {
            "claimed-no-event" => "claimed",
            "active-no-event" => "active",
            _ => "open",
        });
        proof["latest_event"] = Value::Null;
        proof["read_cut"]["position"] = json!(0);
    }
    let decoded: EngramNamedRootReadResponse = serde_json::from_value(proof.clone()).unwrap();
    decoded.validate_authority(&store, &binding).unwrap();
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .root_reads
        .insert(binding.claim_id.clone(), proof);
    claimed
        .state
        .reconcile_engram_named_root(
            &claimed.session_id,
            &token,
            Some(&binding),
            Some(EngramNamedRootState::None),
            u64::MAX,
        )
        .unwrap();
    let inner = claimed.state.inner.lock().unwrap();
    assert!(
        !engram_work_naming_is_current(&inner, &store, &before),
        "first-contact None must recover remote Ended history before admitting an old workdir token"
    );
    let initialized = engram_work_naming_token(&inner, &store, &binding.work_id);
    assert!(
        engram_work_naming_is_current(&inner, &store, &initialized),
        "{case}: acknowledged initialization is usable"
    );
    assert_eq!(inner.engram_work_naming_history[0].proofs[0].read, decoded);
    assert_eq!(
        inner.engram_work_naming_history[0].frontier.is_none(),
        case.ends_with("no-event")
    );
    drop(inner);
    let restored = load_state(claimed.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(
        !engram_work_naming_is_current(&restored, &store, &before),
        "restoration cannot erase the recovered first-contact naming history"
    );
}

#[test]
fn source_root_authority_review_failed_prior_run_is_recoverable_after_claim_change() {
    let label = "authority-recovery-drain";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let tree = add_claimed_root_worktree(&claimed.root);
    name_root(&claimed, label, Some(&tree), vec![claimed_root_held(label)]).unwrap();
    let store = claimed_root_store(&claimed);
    let first = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    let target = AppState::engram_binding_target_for_session_shape_locked(
        &claimed.state.inner.lock().unwrap(),
        &claimed.session_id,
        true,
    )
    .unwrap()
    .unwrap();
    claimed
        .state
        .guard_engram_root_read(&target, Some(&first), Duration::from_secs(2))
        .unwrap();
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .root_read_failures
        .insert(first.claim_id.clone());
    assert!(
        claimed
            .state
            .read_engram_authority_fact(&target, &first, Duration::from_secs(2))
            .is_err()
    );
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .root_read_failures
        .remove(&first.claim_id);
    let mut successor = first.clone();
    successor.claim_id = "recovery-successor-claim".to_owned();
    successor.run_id = "recovery-successor-run".to_owned();
    successor.root_execution_id = "recovery-successor-execution".to_owned();
    claimed.record(|record| {
        record.engram.work_binding = Some(successor.clone());
        record.engram.named_root = Some(EngramNamedRootState::None);
    });
    claimed
        .transport
        .enable_named_roots(&claimed.session_id, &successor);
    let token = claimed.record(|record| record.engram.routing_token.clone().unwrap());
    claimed
        .state
        .reconcile_engram_named_root(
            &claimed.session_id,
            &token,
            Some(&successor),
            Some(EngramNamedRootState::None),
            u64::MAX,
        )
        .unwrap();
    let inner = claimed.state.inner.lock().unwrap();
    assert!(
        !engram_authority_work_unresolved(&inner, &store, &first.work_id),
        "a readable prior run must drain through recovery; fresh successor evaluation cannot remain permanently withheld"
    );
}

#[test]
fn source_root_authority_review_no_event_cannot_erase_unexplained_positive_legacy_history() {
    let label = "authority-legacy-origin";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    prepare_claimed_root_naming(&claimed, label);
    let store = claimed_root_store(&claimed);
    let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    engram_record_naming_history(
        &mut claimed.state.inner.lock().unwrap(),
        &store,
        &binding.work_id,
        7,
    );
    let old = engram_work_naming_token(
        &claimed.state.inner.lock().unwrap(),
        &store,
        &binding.work_id,
    );
    let target = AppState::engram_binding_target_for_session_shape_locked(
        &claimed.state.inner.lock().unwrap(),
        &claimed.session_id,
        true,
    )
    .unwrap()
    .unwrap();
    let no_event = claimed
        .state
        .read_engram_authority_fact(&target, &binding, Duration::from_secs(2))
        .unwrap();
    assert!(
        engram_learn_canonical_history(
            &mut claimed
                .state
                .inner
                .lock()
                .unwrap()
                .engram_work_naming_history[0],
            &no_event
        )
        .is_err()
    );
    let delta = collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
    let mut cache = SqlitePersistConnectionCache::new();
    persist_delta_via_cache(&mut cache, claimed.state.persistence_path.as_path(), &delta).unwrap();
    claimed
        .state
        .recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    let inner = claimed.state.inner.lock().unwrap();
    let history = &inner.engram_work_naming_history[0];
    assert_eq!(history.epoch, 0);
    assert_eq!(history.known_generation, 7);
    assert!(history.frontier.is_none());
    assert!(
        history
            .recovery_reason
            .as_deref()
            .unwrap()
            .contains("legacy naming origin")
    );
    assert!(!engram_work_naming_is_current(&inner, &store, &old));
    assert!(engram_authority_work_unresolved(
        &inner,
        &store,
        &binding.work_id
    ));
    let index = inner.find_session_index(&claimed.session_id).unwrap();
    assert!(
        inner.sessions[index]
            .engram
            .pending_source_root_line
            .as_deref()
            .unwrap()
            .contains("requires repair")
    );
    drop(inner);
    let restored = load_state(claimed.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(restored.engram_work_naming_history[0].known_generation, 7);
    assert!(engram_authority_work_unresolved(
        &restored,
        &store,
        &binding.work_id
    ));
}

#[test]
fn source_root_authority_review_admitted_retirement_waits_for_connected_ack() {
    let label = "authority-admitted-retirement-ack";
    let (claimed, _, _) = named_root_turn(label, true);
    claimed
        .state
        .install_test_engram_budget_clock(EngramBudgetClock::Real);
    let journal = claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_named_root_journal[0]
        .clone();
    let mut proof = covering_removed_root_read(&journal, false);
    proof["run"]["state"] = json!("open");
    proof["latest_event"]["kind"] = json!("ended");
    proof["latest_event"]["event"] = json!("retirement-ended-event");
    proof["latest_event"]["position"]["position"] = json!(2);
    proof["read_cut"]["position"] = json!(2);
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .root_reads
        .insert(journal.claim_id.clone(), proof);
    let (binding, token) = claimed.record(|record| {
        (
            record.engram.work_binding.clone().unwrap(),
            record.engram.routing_token.clone().unwrap(),
        )
    });
    let mut state = claimed.state.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    state.persist_tx = tx;
    let worker = state.clone();
    let session = claimed.session_id.clone();
    let task = std::thread::spawn(move || {
        worker.reconcile_engram_named_root(
            &session,
            &token,
            Some(&binding),
            Some(EngramNamedRootState::None),
            u64::MAX,
        )
    });
    let mut batch = PersistFenceBatch::default();
    receive_naming_authority_fence(&rx, &mut batch, &task);
    let mut cache = SqlitePersistConnectionCache::new();
    let prepared = collect_persist_delta_from_shared_state(&state.inner, 0);
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &prepared,
        &mut batch,
    )
    .unwrap();
    receive_naming_authority_fence(&rx, &mut batch, &task);
    let capture_before_ack = state.engram_turn_root_capture(&claimed.session_id);
    batch.fail(PersistFenceError::WriteFailed(
        "retirement commit failure".to_owned(),
    ));
    assert!(task.join().unwrap().is_err());
    let capture_after_failure = state.engram_turn_root_capture(&claimed.session_id);
    assert_eq!(
        capture_before_ack,
        EngramRootCapture::Unconfirmed,
        "immutable admitted identity must not release checkpoint evidence before candidate acknowledgement"
    );
    assert_eq!(
        capture_after_failure,
        EngramRootCapture::Unconfirmed,
        "failed retirement settlement must retain evidence withholding"
    );
}

#[test]
fn source_root_authority_review_staging_keeps_sqlite_wait_off_state_lock() {
    authority_review_staging_writer_lock(false);
}

#[test]
fn source_root_authority_review_cleanup_assignment_keeps_sqlite_wait_off_state_lock() {
    authority_review_staging_writer_lock(true);
}

fn authority_review_staging_writer_lock(cleanup: bool) {
    let label = if cleanup {
        "authority-cleanup-stage-lock"
    } else {
        "authority-event-stage-lock"
    };
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let tree = add_claimed_root_worktree(&claimed.root);
    name_root(&claimed, label, Some(&tree), vec![claimed_root_held(label)]).unwrap();
    let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    let root = claimed.state.inner.lock().unwrap().engram_work_source_roots[0].clone();
    let event = EngramNamedRootEvent {
        root: root.clone(),
        reporter: claimed.session_id.clone(),
        kind: EngramNamedRootKind::Ended,
        end_reason: Some(EngramNamedRootEndReason::RootInvalid),
    };
    if cleanup {
        let mut inner = claimed.state.inner.lock().unwrap();
        engram_queue_root_cleanup(
            &mut inner.engram_named_root_journal,
            &root,
            EngramNamedRootEndReason::RootInvalid,
        );
    }
    let writer = sqlite_state_write_lock(claimed.state.persistence_path.as_path());
    let held_writer = lock_sqlite_state_writer(&writer);
    let ticket = sqlite_state_writer_issued_tickets(&writer) + 1;
    let state = claimed.state.clone();
    let session = claimed.session_id.clone();
    let task = std::thread::spawn(move || {
        if cleanup {
            state.flush_engram_root_cleanup(&session, Duration::from_secs(2));
        } else {
            let mut inner = state.inner.lock().unwrap();
            state
                .stage_engram_root_event_locked(&mut inner, &event, Some(&binding))
                .unwrap();
            drop(inner);
            // Staging itself no longer writes. Drive its production owner
            // path to the same real SQLite admission witness off-lock.
            let target = AppState::engram_binding_target_for_session_shape_locked(
                &state.inner.lock().unwrap(),
                &session,
                true,
            )
            .unwrap()
            .unwrap();
            state
                .send_engram_root_event(&target, &event, Duration::from_secs(2))
                .unwrap();
        }
    });
    wait_for_sqlite_state_writer_issued_tickets(&writer, ticket);
    let state_available = claimed.state.inner.inner.try_lock().is_ok();
    drop(held_writer);
    task.join().unwrap();
    assert!(
        state_available,
        "staging/cleanup waited on the real SQLite writer while holding StateInner"
    );
}

#[test]
fn source_root_authority_disconnected_preparation_does_not_hold_state_while_writing() {
    authority_disconnected_writer_lock_independence(false);
}

#[test]
fn source_root_authority_disconnected_candidate_does_not_hold_state_while_writing() {
    authority_disconnected_writer_lock_independence(true);
}

fn authority_disconnected_writer_lock_independence(candidate: bool) {
    let label = if candidate {
        "authority-candidate-lock"
    } else {
        "authority-prepared-lock"
    };
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let tree = add_claimed_root_worktree(&claimed.root);
    name_root(&claimed, label, Some(&tree), vec![claimed_root_held(label)]).unwrap();
    let store = claimed_root_store(&claimed);
    let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    let owner = candidate.then(|| {
        claimed
            .state
            .prepare_engram_authority(&store, &binding, Duration::from_secs(2))
            .unwrap()
    });
    let writer = sqlite_state_write_lock(claimed.state.persistence_path.as_path());
    let held_writer = lock_sqlite_state_writer(&writer);
    let ticket = sqlite_state_writer_issued_tickets(&writer) + 1;
    let state = claimed.state.clone();
    let task = std::thread::spawn(move || {
        if let Some(owner) = owner {
            state.publish_engram_authority(&store, &owner, Duration::from_secs(2))
        } else {
            state
                .prepare_engram_authority(&store, &binding, Duration::from_secs(2))
                .map(|_| ())
        }
    });
    // The real SQLite writer admission proves the write is waiting. No timing
    // guess or replacement persistence callback stands in for that path.
    wait_for_sqlite_state_writer_issued_tickets(&writer, ticket);
    let state_available = claimed.state.inner.inner.try_lock().is_ok();
    drop(held_writer);
    task.join().unwrap().unwrap();
    assert!(
        state_available,
        "authority persistence waited on SQLite while holding StateInner"
    );
}

#[test]
fn source_root_authority_renewed_claim_settles_the_same_canonical_run() {
    let label = "authority-renewed-claim";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let tree = add_claimed_root_worktree(&claimed.root);
    name_root(&claimed, label, Some(&tree), vec![claimed_root_held(label)]).unwrap();
    let store = claimed_root_store(&claimed);
    let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    claimed
        .state
        .prepare_engram_authority(&store, &binding, Duration::from_secs(2))
        .unwrap();
    let mut renewed = binding.clone();
    renewed.work_revision += 1;
    renewed.claim_fence += 1;
    let owner = claimed
        .state
        .prepare_engram_authority(&store, &renewed, Duration::from_secs(2))
        .unwrap();
    let target = AppState::engram_binding_target_for_session_shape_locked(
        &claimed.state.inner.lock().unwrap(),
        &claimed.session_id,
        true,
    )
    .unwrap()
    .unwrap();
    let proof = claimed
        .state
        .read_engram_authority_fact(&target, &renewed, Duration::from_secs(2))
        .unwrap();
    AppState::learn_engram_authority_locked(
        &mut claimed.state.inner.lock().unwrap(),
        &store,
        &owner,
        &proof,
    )
    .unwrap();
    claimed
        .state
        .publish_engram_authority(&store, &owner, Duration::from_secs(2))
        .unwrap();
    let inner = claimed.state.inner.lock().unwrap();
    let token = engram_work_naming_token(&inner, &store, &binding.work_id);
    assert!(
        engram_work_naming_is_current(&inner, &store, &token),
        "renewing a fence does not create another canonical run recovery obligation"
    );
}

#[test]
fn source_root_authority_late_ack_cannot_publish_a_successor_owner() {
    connected_authority_candidate_ack(true);
}

#[test]
fn source_root_authority_unrelated_work_does_not_starve_its_content_fence() {
    connected_authority_candidate_ack(false);
}

fn connected_authority_candidate_ack(supersede: bool) {
    let label = if supersede {
        "authority-late-ack"
    } else {
        "authority-unrelated-work"
    };
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let tree = add_claimed_root_worktree(&claimed.root);
    name_root(&claimed, label, Some(&tree), vec![claimed_root_held(label)]).unwrap();
    let store = claimed_root_store(&claimed);
    let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    let owner = claimed
        .state
        .prepare_engram_authority(&store, &binding, Duration::from_secs(2))
        .unwrap();
    let target = AppState::engram_binding_target_for_session_shape_locked(
        &claimed.state.inner.lock().unwrap(),
        &claimed.session_id,
        true,
    )
    .unwrap()
    .unwrap();
    let proof = claimed
        .state
        .read_engram_authority_fact(&target, &binding, Duration::from_secs(2))
        .unwrap();
    AppState::learn_engram_authority_locked(
        &mut claimed.state.inner.lock().unwrap(),
        &store,
        &owner,
        &proof,
    )
    .unwrap();
    claimed
        .state
        .install_test_engram_budget_clock(EngramBudgetClock::Real);
    let mut state = claimed.state.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    state.persist_tx = tx;
    let worker = state.clone();
    let worker_store = store.clone();
    let task = std::thread::spawn(move || {
        worker.publish_engram_authority(&worker_store, &owner, Duration::from_secs(2))
    });
    let mut batch = PersistFenceBatch::default();
    loop {
        let request = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let fence = matches!(request, PersistRequest::Fence(_));
        batch.accept(request);
        if fence {
            break;
        }
    }
    {
        let mut inner = state.inner.lock().unwrap();
        if !supersede {
            let mut other = inner.engram_work_naming_history[0].clone();
            other.work_id = "unrelated-work".to_owned();
            other.transition = None;
            other.unresolved_runs.clear();
            inner.engram_work_naming_history.push(other);
            inner.engram_source_root_generation += 1;
        }
    }
    let delta = collect_persist_delta_from_shared_state(&state.inner, 0);
    let mut cache = SqlitePersistConnectionCache::new();
    persist_delta_via_cache(&mut cache, state.persistence_path.as_path(), &delta).unwrap();
    if supersede {
        // A real successor's version is independent of the prior SQLite
        // transaction. Its late acknowledgement must not publish that owner.
        let mut inner = state.inner.lock().unwrap();
        let history = inner
            .engram_work_naming_history
            .iter_mut()
            .find(|history| history.work_id == binding.work_id)
            .unwrap();
        history.owner_version += 1;
        let successor = history.transition.as_mut().unwrap();
        successor.id = "successor-owner".to_owned();
        successor.version = history.owner_version;
        successor.phase = EngramAuthorityPhase::Prepared;
    }
    batch.finish_write(&delta, &Ok(Vec::new()), cache.connection.as_ref());
    let result = task.join().unwrap();
    let inner = state.inner.lock().unwrap();
    let token = engram_work_naming_token(&inner, &store, &binding.work_id);
    if supersede {
        assert!(result.is_err());
        assert!(!engram_work_naming_is_current(&inner, &store, &token));
        assert_eq!(
            inner.engram_work_naming_history[0]
                .transition
                .as_ref()
                .unwrap()
                .id,
            "successor-owner"
        );
    } else {
        result.unwrap();
        assert!(engram_work_naming_is_current(&inner, &store, &token));
    }
}

#[test]
fn source_root_authority_canonical_frontier_orders_runs_and_events() {
    let label = "authority-canonical-clock";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let tree = add_claimed_root_worktree(&claimed.root);
    name_root(&claimed, label, Some(&tree), vec![claimed_root_held(label)]).unwrap();
    let inner = claimed.state.inner.lock().unwrap();
    let mut history = inner.engram_work_naming_history[0].clone();
    let journal = inner.engram_named_root_journal[0].clone();
    drop(inner);
    let mut raw = covering_removed_root_read(&journal, true);
    raw["run"]["generation"] = json!(1);
    let proof: EngramNamedRootReadResponse = serde_json::from_value(raw.clone()).unwrap();
    let revision = history.revision;
    engram_learn_canonical_history(&mut history, &proof).unwrap();
    assert_eq!(
        history.revision, revision,
        "release/cut progression is not a distinct naming event"
    );
    let mut same_position = proof.clone();
    same_position.latest_event.as_mut().unwrap().event =
        "different-event-at-the-same-position".to_owned();
    assert!(engram_learn_canonical_history(&mut history, &same_position).is_err());
    let mut same_ordinal = proof.clone();
    same_ordinal.run_id = "different-run-at-the-same-ordinal".to_owned();
    assert!(engram_learn_canonical_history(&mut history, &same_ordinal).is_err());
    let mut ended = proof.clone();
    ended.latest_event.as_mut().unwrap().kind = EngramNamedRootKind::Ended;
    ended.latest_event.as_mut().unwrap().event = "later-known-ended-event".to_owned();
    ended.latest_event.as_mut().unwrap().position.position += 1;
    ended.named_root = EngramNamedRootState::None;
    ended.read_cut.position = ended.latest_event.as_ref().unwrap().position.position;
    ended
        .validate_authority(&journal.store, journal.read_binding.as_ref().unwrap())
        .unwrap();
    engram_learn_canonical_history(&mut history, &ended).unwrap();
    assert_eq!(
        history.revision, revision,
        "known binding cleanup only changes lifecycle proof"
    );
    let mut successor = proof.clone();
    successor.run.generation = 2;
    successor.run_id = "canonical-successor-run".to_owned();
    successor.claim_id = "canonical-successor-claim".to_owned();
    successor.root_execution_id = "canonical-successor-execution".to_owned();
    successor.read_cut.feed.id = successor.run_id.clone();
    successor.latest_event.as_mut().unwrap().position.feed.id = successor.run_id.clone();
    successor.latest_event.as_mut().unwrap().event = "successor-name".to_owned();
    successor.latest_event.as_mut().unwrap().generation = 1;
    engram_learn_canonical_history(&mut history, &successor).unwrap();
    assert_eq!(
        history.revision,
        revision + 1,
        "a later canonical run may restart its root numbering"
    );
    engram_learn_canonical_history(&mut history, &ended).unwrap();
    assert_eq!(
        history.revision,
        revision + 1,
        "late older-run cleanup cannot lower or revive history"
    );
    let mut unknown_end = successor.clone();
    unknown_end.latest_event.as_mut().unwrap().position.position += 1;
    unknown_end.latest_event.as_mut().unwrap().event = "unknown-later-ended-name".to_owned();
    unknown_end.latest_event.as_mut().unwrap().generation = 2;
    unknown_end.latest_event.as_mut().unwrap().kind = EngramNamedRootKind::Ended;
    unknown_end.named_root = EngramNamedRootState::None;
    unknown_end.read_cut.position = unknown_end.latest_event.as_ref().unwrap().position.position;
    engram_learn_canonical_history(&mut history, &unknown_end).unwrap();
    assert_eq!(
        history.revision,
        revision + 2,
        "an unknown Ended event teaches another naming"
    );
}

#[test]
fn source_root_authority_connected_reconciliation_keeps_failed_commit_unknown() {
    connected_authority_settlement_failure("reconcile");
}

#[test]
fn source_root_authority_connected_replay_keeps_failed_commit_unknown() {
    connected_authority_settlement_failure("replay");
}

#[test]
fn source_root_authority_connected_cleanup_keeps_failed_commit_unknown() {
    connected_authority_settlement_failure("cleanup");
}

fn connected_authority_settlement_failure(entry: &'static str) {
    let label = format!("authority-commit-{entry}");
    let claimed = ClaimedRoot::new(&label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_claimed_root_naming(&claimed, &label);
    if entry == "replay" {
        claimed
            .transport
            .named_roots
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .lose_next_reply = true;
        name_root(
            &claimed,
            &label,
            Some(&worktree),
            vec![claimed_root_held(&label)],
        )
        .unwrap_err();
    } else {
        name_root(
            &claimed,
            &label,
            Some(&worktree),
            vec![claimed_root_held(&label)],
        )
        .unwrap();
    }
    let store = claimed_root_store(&claimed);
    let (binding, token) = claimed.record(|record| {
        (
            record.engram.work_binding.clone().unwrap(),
            record.engram.routing_token.clone().unwrap(),
        )
    });
    let intent = {
        let mut inner = claimed.state.inner.lock().unwrap();
        if entry == "cleanup" {
            let root = inner.engram_work_source_roots[0].clone();
            engram_queue_root_cleanup(
                &mut inner.engram_named_root_journal,
                &root,
                EngramNamedRootEndReason::RootInvalid,
            );
        }
        claimed.state.persist_internal_locked(&inner).unwrap();
        inner.engram_named_root_journal[0].pending.clone()
    };
    let real_receipt: EngramNamedRootReceipt = serde_json::from_value(
        claimed
            .transport
            .named_roots
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .seen
            .values()
            .next()
            .unwrap()
            .1
            .clone(),
    )
    .unwrap();
    if entry == "reconcile" {
        let journal = claimed
            .state
            .inner
            .lock()
            .unwrap()
            .engram_named_root_journal[0]
            .clone();
        let mut proof = covering_removed_root_read(&journal, true);
        proof["latest_event"]["event"] = json!("later-canonical-name");
        proof["latest_event"]["generation"] = json!(7);
        proof["latest_event"]["position"]["position"] = json!(10);
        proof["named_root"]["last_generation"] = json!(7);
        proof["named_root"]["released_at_position"] = json!(100);
        proof["read_cut"]["position"] = json!(100);
        claimed
            .transport
            .named_roots
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .root_reads
            .insert(journal.claim_id.clone(), proof);
    }
    let mut state = claimed.state.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    state.persist_tx = tx;
    let worker_state = state.clone();
    let session = claimed.session_id.clone();
    let work = binding.work_id.clone();
    let task = std::thread::spawn(move || match entry {
        "reconcile" => worker_state
            .reconcile_engram_named_root(
                &session,
                &token,
                Some(&binding),
                Some(EngramNamedRootState::UnboundByRelease {
                    last_generation: 7,
                    released_at_position: 100,
                }),
                u64::MAX,
            )
            .map_err(|error| error.to_string()),
        "replay" => worker_state
            .retire_engram_root_replay(&intent.unwrap(), &real_receipt, None)
            .map_err(|error| error.message),
        "cleanup" => {
            worker_state.flush_engram_root_cleanup(&session, Duration::from_secs(2));
            Ok(())
        }
        _ => unreachable!(),
    });
    let mut batch = PersistFenceBatch::default();
    let mut cache = SqlitePersistConnectionCache::new();
    let mut watermark = 0;
    let mut failed_candidate = false;
    for _ in 0..6 {
        let mut has_fence = false;
        for _ in 0..50 {
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(request) => {
                    has_fence = matches!(request, PersistRequest::Fence(_));
                    batch.accept(request);
                    if has_fence {
                        break;
                    }
                }
                Err(_) if task.is_finished() => break,
                Err(_) => {}
            }
        }
        if !has_fence {
            break;
        }
        let inner = state.inner.lock().unwrap();
        let token = engram_work_naming_token(&inner, &store, &work);
        assert!(
            !engram_work_naming_is_current(&inner, &store, &token),
            "{entry}: every awaited authority image remains withheld"
        );
        drop(inner);
        let delta = collect_persist_delta_from_shared_state(&state.inner, watermark);
        // The actual SQLite transaction establishes a crash image BEFORE the
        // worker acknowledges it; a channel/fence alone cannot prove this.
        if cache.connection.is_none() {
            persist_delta_via_cache(&mut cache, state.persistence_path.as_path(), &delta).unwrap();
            let disk = load_state(state.persistence_path.as_path())
                .unwrap()
                .unwrap();
            let durable_token = engram_work_naming_token(&disk, &store, &work);
            assert!(
                !engram_work_naming_is_current(&disk, &store, &durable_token),
                "{entry}: commit-before-ack reconstruction cannot release the work guard"
            );
            batch.finish_write(&delta, &Ok(Vec::new()), cache.connection.as_ref());
            watermark = delta.watermark;
            continue;
        }
        assert!(
            claimed
                .transport
                .requests()
                .iter()
                .any(|request| request.request["operation"] == "named_root_read"),
            "{entry}: the failure must happen after a canonical response was learned"
        );
        // Commit the candidate without delivering its acknowledgement, then
        // fail a real subsequent writer transaction on the same connection.
        persist_delta_via_cache(&mut cache, state.persistence_path.as_path(), &delta).unwrap();
        let disk = load_state(state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        let durable_token = engram_work_naming_token(&disk, &store, &work);
        assert!(
            !engram_work_naming_is_current(&disk, &store, &durable_token),
            "{entry}: a candidate committed without acknowledgement still restores withheld"
        );
        cache.connection.as_ref().unwrap().execute_batch(
            "CREATE TEMP TRIGGER refuse_authority_metadata BEFORE INSERT ON app_state BEGIN SELECT RAISE(ABORT, 'authority commit failure'); END;").unwrap();
        let failure = persist_delta_with_fences(
            &mut cache,
            state.persistence_path.as_path(),
            &delta,
            &mut batch,
        );
        assert!(
            failure.is_err(),
            "real connected writer must fail its metadata transaction"
        );
        failed_candidate = true;
        break;
    }
    // Release any pending fence on fixture failure so no worker is orphaned.
    batch.fail(PersistFenceError::WorkerStopped);
    task.join().unwrap().ok();
    assert!(
        failed_candidate,
        "{entry}: settlement returned without waiting for a complete durable authority image"
    );
    let inner = state.inner.lock().unwrap();
    let token = engram_work_naming_token(&inner, &store, &work);
    assert!(
        !engram_work_naming_is_current(&inner, &store, &token),
        "{entry}: a failed or ambiguous writer never restores usable old authority"
    );
}

#[test]
fn source_root_authority_foreign_generation_cannot_hide_a_later_claim_name() {
    let label = "authority-cross-claim-history";
    let mut claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    replace_naming_session(&mut claimed, label);
    let store = claimed_root_store(&claimed);
    let old = claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_named_root_journal[0]
        .clone();
    let mut read = covering_removed_root_read(&old, true);
    read["run"]["generation"] = json!(1);
    read["latest_event"]["event"] = json!("foreign-naming-event");
    read["latest_event"]["generation"] = json!(100);
    read["latest_event"]["position"]["position"] = json!(10);
    read["named_root"]["last_generation"] = json!(100);
    read["named_root"]["released_at_position"] = json!(11);
    read["read_cut"]["position"] = json!(11);
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .root_reads
        .insert(old.claim_id.clone(), read);
    claimed
        .state
        .resolve_removed_engram_roots(&claimed.session_id, None, Duration::from_secs(2));

    let mut held = claimed_root_held(label);
    let binding = held.items[0].control_binding.as_mut().unwrap();
    binding.claim_id = "successor-claim".to_owned();
    binding.run_id = "successor-run".to_owned();
    binding.root_execution_id = "successor-execution".to_owned();
    let binding = binding.clone();
    held.items[0].claim_id = binding.claim_id.clone();
    claimed.record(|record| {
        record.engram.work_binding = Some(binding.clone());
        record.engram.named_root = Some(EngramNamedRootState::None);
    });
    claimed
        .transport
        .enable_named_roots(&claimed.session_id, &binding);
    let token = claimed.record(|record| record.engram.routing_token.clone().unwrap());
    claimed
        .reconcile_root_readback(
            &claimed.session_id,
            &token,
            Some(&binding),
            Some(EngramNamedRootState::None),
            u64::MAX,
        )
        .unwrap();
    let prepared = engram_work_naming_token(
        &claimed.state.inner.lock().unwrap(),
        &store,
        &binding.work_id,
    );
    let seed = AcceptanceEvaluationTargetSeed {
        work_ref: format!("w-{label}"),
        mode: AcceptanceEvaluationMode::IndependentSession,
        acceptance_basis: 1,
        evidence_basis: 1,
        criteria_count: 1,
        bindings: Vec::new(),
        supersedes: None,
        store: store.clone(),
        source_fingerprint: None,
        source_root: None,
        source_claim: Some(AcceptanceEvaluationSourceClaim {
            work_id: binding.work_id.clone(),
            claim_id: binding.claim_id.clone(),
            named_generation_at_request: None,
        }),
        work_id: Some(binding.work_id.clone()),
        naming_history: Some(prepared.clone()),
    };
    acceptance_evaluation_spawn_admission_locked(
        &claimed.state.inner.lock().unwrap(),
        &claimed.session_id,
        &seed,
    )
    .expect("fresh unbound successor initially admits a workdir evaluation");
    let request = |path: Option<&FsPath>| {
        claimed.transport.replace_held_claims([Ok(held.clone())]);
        claimed.state.name_engram_source_root(
            &claimed.session_id,
            EngramSourceRootRequest {
                work: format!("w-{label}"),
                path: path.map(|path| Some(path.to_string_lossy().into_owned())),
            },
        )
    };
    let named = request(Some(&worktree)).unwrap();
    assert!(
        named.generation < 100,
        "claim-local protocol numbers need not exceed another claim's number"
    );
    request(None).unwrap();
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        engram_compact_root_journal(&mut inner);
        assert!(
            !engram_work_naming_is_current(&inner, &store, &prepared),
            "a distinct successor naming must invalidate the original workdir preparation after clear and compaction"
        );
        acceptance_evaluation_spawn_admission_locked(&inner, &claimed.session_id, &seed)
            .expect_err("the original seed cannot spawn after later naming");
        assert!(
            acceptance_evaluation_root_changed_locked(
                &inner,
                &seed.clone().into_target("old-request".to_owned())
            ),
            "first submission keeps the original naming token"
        );
    }
    let restored = load_state(claimed.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(
        !engram_work_naming_is_current(&restored, &store, &prepared),
        "restart cannot revive the old workdir token"
    );
}

#[test]
fn source_root_authority_removed_reader_requires_a_durable_pre_io_guard() {
    let label = "authority-connected-reader";
    let mut claimed = ClaimedRoot::new(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap();
    replace_naming_session(&mut claimed, label);
    let journal = claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_named_root_journal[0]
        .clone();
    claimed.record(|record| {
        let binding = record.engram.work_binding.as_mut().unwrap();
        binding.claim_id = "reader-successor-claim".to_owned();
        binding.run_id = "reader-successor-run".to_owned();
        binding.root_execution_id = "reader-successor-execution".to_owned();
        record.engram.named_root = Some(EngramNamedRootState::None);
    });
    let mut read = covering_removed_root_read(&journal, true);
    read["run"]["generation"] = json!(1);
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .root_reads
        .insert(journal.claim_id.clone(), read);
    {
        let inner = claimed.state.inner.lock().unwrap();
        claimed.state.persist_internal_locked(&inner).unwrap();
    }
    let (tx, rx) = std::sync::mpsc::channel();
    claimed.state.persist_tx = tx;
    let state = claimed.state.clone();
    let session = claimed.session_id.clone();
    let transport = claimed.transport.clone();
    let preceding_reads = transport
        .requests()
        .iter()
        .filter(|r| r.request["operation"] == "named_root_read")
        .count();
    let task = std::thread::spawn(move || {
        state.resolve_removed_engram_roots(&session, None, Duration::from_secs(2))
    });
    let mut batch = PersistFenceBatch::default();
    let mut has_fence = false;
    for _ in 0..50 {
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(request) => {
                has_fence = matches!(request, PersistRequest::Fence(_));
                batch.accept(request);
                if has_fence {
                    break;
                }
            }
            Err(_) if task.is_finished() => break,
            Err(_) => {}
        }
    }
    if !has_fence {
        task.join().unwrap();
        panic!(
            "authoritative lifecycle I/O ran without requesting an acknowledged durable recovery guard"
        );
    }
    assert_eq!(
        transport
            .requests()
            .iter()
            .filter(|r| r.request["operation"] == "named_root_read")
            .count(),
        preceding_reads,
        "the reader must not learn a new fact before its recovery obligation is committed"
    );
    let mut cache = SqlitePersistConnectionCache::new();
    let first = collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
    persist_delta_with_fences(
        &mut cache,
        claimed.state.persistence_path.as_path(),
        &first,
        &mut batch,
    )
    .unwrap();
    let mut candidate_fence = false;
    for _ in 0..50 {
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(request) => {
                candidate_fence = matches!(request, PersistRequest::Fence(_));
                batch.accept(request);
                if candidate_fence {
                    break;
                }
            }
            Err(_) if task.is_finished() => break,
            Err(_) => {}
        }
    }
    if !candidate_fence {
        task.join().unwrap();
        panic!("reader candidate had no content-specific durable acknowledgement");
    }
    assert_eq!(
        claimed.state.engram_root_capture(&claimed.session_id),
        EngramRootCapture::Unconfirmed,
        "a queued unbound candidate cannot release capture authority"
    );
    let durable_prepared = load_state(claimed.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let token = engram_work_naming_token(
        &durable_prepared,
        &journal.store,
        &journal.confirmed.as_ref().unwrap().0.root.work_id,
    );
    assert!(
        !engram_work_naming_is_current(&durable_prepared, &journal.store, &token),
        "reconstructing the actual pre-response durable image retains the work guard"
    );
    let candidate = collect_persist_delta_from_shared_state(&claimed.state.inner, first.watermark);
    persist_delta_with_fences(
        &mut cache,
        claimed.state.persistence_path.as_path(),
        &candidate,
        &mut batch,
    )
    .unwrap();
    task.join().unwrap();
    assert!(
        claimed
            .state
            .inner
            .lock()
            .unwrap()
            .engram_named_root_journal
            .iter()
            .all(|entry| entry.claim_id != journal.claim_id),
        "acknowledged unused retirement is compactable"
    );
}

struct AuthorityTimeoutRecorder {
    control: Arc<ScriptedEngramControlTransport>,
    calls: Mutex<Vec<(String, Duration)>>,
}

impl EngramControlTransport for AuthorityTimeoutRecorder {
    fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.calls
            .lock()
            .unwrap()
            .push(("work_binding_read".to_owned(), timeout));
        self.control
            .read_work_binding(connection, preference, timeout)
    }
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        self.calls.lock().unwrap().push((
            serde_json::to_value(request).unwrap()["operation"]
                .as_str()
                .unwrap()
                .to_owned(),
            timeout,
        ));
        self.control.request(connection, request, timeout)
    }
    fn shutdown_session(&self, session: &str) {
        self.control.shutdown_session(session);
    }
}

#[test]
fn source_root_authority_review_writer_wait_uses_total_headroom_and_preserves_rpc_cap() {
    let label = "authority-total-versus-rpc";
    let claimed = ClaimedRoot::new(label, Vec::new());
    prepare_claimed_root_naming(&claimed, label);
    let store = claimed_root_store(&claimed);
    let recorder = Arc::new(AuthorityTimeoutRecorder {
        control: claimed.transport.clone(),
        calls: Mutex::new(Vec::new()),
    });
    install_control_only_transport(&claimed.state, recorder.clone());
    let (binding, token) = claimed.record(|record| {
        (
            record.engram.work_binding.clone().unwrap(),
            record.engram.routing_token.clone().unwrap(),
        )
    });
    let target = AppState::engram_binding_target_for_session_shape_locked(
        &claimed.state.inner.lock().unwrap(),
        &claimed.session_id,
        true,
    )
    .unwrap()
    .unwrap();
    let rpc = target.settings.call_timeout();
    let writer = sqlite_state_write_lock(claimed.state.persistence_path.as_path());
    let held = lock_sqlite_state_writer(&writer);
    let ticket = sqlite_state_writer_issued_tickets(&writer) + 1;
    let worker = claimed.state.clone();
    let session = claimed.session_id.clone();
    let work = binding.work_id.clone();
    let task = std::thread::spawn(move || {
        let result = worker.reconcile_engram_named_root(
            &session,
            &token,
            Some(&binding),
            Some(EngramNamedRootState::None),
            u64::MAX,
        );
        eprintln!("funded writer worker result: {result:?}");
        result
    });
    wait_for_sqlite_state_writer_issued_tickets(&writer, ticket);
    assert!(claimed.state.inner.inner.try_lock().is_ok());
    // Force the local SQL phase beyond the per-RPC interval. The existing
    // fixture total headroom remains separate; no configured limit is raised.
    std::thread::sleep(rpc + Duration::from_millis(25));
    drop(held);
    task.join().unwrap().unwrap();
    let calls = recorder.calls.lock().unwrap();
    assert!(calls.iter().any(|(op, _)| op == "named_root_read"));
    assert!(
        calls
            .iter()
            .all(|(_, timeout)| !timeout.is_zero() && *timeout <= rpc)
    );
    let inner = claimed.state.inner.lock().unwrap();
    assert!(!engram_authority_work_unresolved(&inner, &store, &work));
}

#[test]
fn source_root_authority_review_expired_admission_does_not_fall_back_to_standalone_budget() {
    let label = "authority-expired-admission";
    let claimed = ClaimedRoot::new(label, Vec::new());
    prepare_claimed_root_naming(&claimed, label);
    let mut target = AppState::engram_binding_target_for_session_shape_locked(
        &claimed.state.inner.lock().unwrap(),
        &claimed.session_id,
        true,
    )
    .unwrap()
    .unwrap();
    let now = std::time::Instant::now();
    let total = target.test_dispatch_budget.unwrap();
    target.admission_started_at = Some(now - total - Duration::from_millis(1));
    let expired = target.root_operation_deadline(now);
    assert!(expired < now);
    assert!(target.rpc_timeout_until(expired).is_err());
    target.admission_started_at = None;
    assert_eq!(
        target.root_operation_deadline(now),
        now + target.settings.call_timeout()
    );
    assert!(
        target
            .rpc_timeout_until(target.root_operation_deadline(now))
            .is_ok()
    );
    assert!(claimed.transport.requests().is_empty());
}

#[test]
fn source_root_authority_review_no_event_keeps_potentially_executable_pending_write() {
    let label = "authority-no-event-pending-write";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    prepare_claimed_root_naming(&claimed, label);
    let store = claimed_root_store(&claimed);
    let (binding, token) = claimed.record(|record| {
        (
            record.engram.work_binding.clone().unwrap(),
            record.engram.routing_token.clone().unwrap(),
        )
    });
    claimed
        .state
        .reconcile_engram_named_root(
            &claimed.session_id,
            &token,
            Some(&binding),
            Some(EngramNamedRootState::None),
            u64::MAX,
        )
        .unwrap();
    let event = EngramNamedRootEvent {
        root: EngramWorkSourceRoot {
            store: store.clone(),
            work_id: binding.work_id.clone(),
            short_ref: label.to_owned(),
            claim_id: binding.claim_id.clone(),
            claim_fence: binding.claim_fence,
            root: claimed.root.to_string_lossy().into_owned(),
            common_dir_key: "fixture-common-dir".to_owned(),
            named_by_session: "unavailable-reporter".to_owned(),
            named_at: "2026-10-01T00:00:00Z".to_owned(),
            generation: 1,
        },
        reporter: "unavailable-reporter".to_owned(),
        kind: EngramNamedRootKind::Bound,
        end_reason: None,
    };
    claimed
        .state
        .stage_engram_root_event_locked(
            &mut claimed.state.inner.lock().unwrap(),
            &event,
            Some(&binding),
        )
        .unwrap();
    let original = serde_json::to_value(event.request("original-routing-token").unwrap()).unwrap();
    claimed
        .state
        .reconcile_engram_named_root(
            &claimed.session_id,
            &token,
            Some(&binding),
            Some(EngramNamedRootState::None),
            u64::MAX,
        )
        .unwrap();
    let inner = claimed.state.inner.lock().unwrap();
    let journal =
        engram_root_journal(&inner.engram_named_root_journal, &store, &binding.claim_id).unwrap();
    assert_eq!(journal.pending.as_ref(), Some(&event));
    assert_eq!(
        serde_json::to_value(
            journal
                .pending
                .as_ref()
                .unwrap()
                .request("original-routing-token")
                .unwrap()
        )
        .unwrap(),
        original
    );
    assert!(!journal.reconciliation.as_ref().unwrap().settled);
    assert!(engram_authority_work_unresolved(
        &inner,
        &store,
        &binding.work_id
    ));
    assert_eq!(
        engram_root_capture_locked(&inner, &claimed.session_id),
        EngramRootCapture::Unconfirmed
    );
    drop(inner);
    let restored = load_state(claimed.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(engram_authority_work_unresolved(
        &restored,
        &store,
        &binding.work_id
    ));
    assert_eq!(
        restored.engram_named_root_journal[0].pending.as_ref(),
        Some(&event)
    );
    assert!(
        !claimed
            .transport
            .requests()
            .iter()
            .any(|request| request.request["operation"] == "named_root_bind")
    );
}

#[test]
fn source_root_authority_review_stopped_production_writer_refuses_staging_and_cleanup_without_sqlite()
 {
    for cleanup in [false, true] {
        let label = if cleanup {
            "authority-stopped-cleanup"
        } else {
            "authority-stopped-stage"
        };
        let claimed = ClaimedRoot::new_scripted(label, Vec::new());
        let tree = add_claimed_root_worktree(&claimed.root);
        name_root(&claimed, label, Some(&tree), vec![claimed_root_held(label)]).unwrap();
        let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
        let root = claimed.state.inner.lock().unwrap().engram_work_source_roots[0].clone();
        let event = EngramNamedRootEvent {
            root: root.clone(),
            reporter: claimed.session_id.clone(),
            kind: EngramNamedRootKind::Ended,
            end_reason: Some(EngramNamedRootEndReason::RootInvalid),
        };
        let before = claimed.transport.requests().len();
        if cleanup {
            engram_queue_root_cleanup(
                &mut claimed
                    .state
                    .inner
                    .lock()
                    .unwrap()
                    .engram_named_root_journal,
                &root,
                EngramNamedRootEndReason::RootInvalid,
            );
        }
        let writer = sqlite_state_write_lock(claimed.state.persistence_path.as_path());
        let held = lock_sqlite_state_writer(&writer);
        let tickets_before = sqlite_state_writer_issued_tickets(&writer);
        let state = claimed.state.clone();
        let session = claimed.session_id.clone();
        let retained = event.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let task = std::thread::spawn(move || {
            TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(false));
            if cleanup {
                state.flush_engram_root_cleanup(&session, Duration::from_secs(2));
            } else {
                state
                    .stage_engram_root_event_locked(
                        &mut state.inner.lock().unwrap(),
                        &event,
                        Some(&binding),
                    )
                    .unwrap();
                let target = AppState::engram_binding_target_for_session_shape_locked(
                    &state.inner.lock().unwrap(),
                    &session,
                    true,
                )
                .unwrap()
                .unwrap();
                let error = state
                    .send_engram_root_event(&target, &event, Duration::from_secs(2))
                    .err()
                    .unwrap();
                assert!(error.message.contains("writer stopped"), "{error:?}");
            }
            done_tx.send(()).unwrap();
        });
        let completed_while_writer_locked = done_rx.recv_timeout(DEADLOCK_GUARD).is_ok();
        let state_available = claimed.state.inner.inner.try_lock().is_ok();
        let tickets_after = sqlite_state_writer_issued_tickets(&writer);
        drop(held);
        task.join().unwrap();
        assert!(completed_while_writer_locked && state_available);
        assert_eq!(
            tickets_before, tickets_after,
            "production refusal must not enter SQLite fallback"
        );
        assert_eq!(
            claimed.transport.requests().len(),
            before,
            "no authority-changing transport without Prepared ACK"
        );
        let inner = claimed.state.inner.lock().unwrap();
        assert_eq!(
            inner.engram_named_root_journal[0].pending.as_ref(),
            Some(&retained)
        );
        assert!(engram_authority_work_unresolved(
            &inner,
            &root.store,
            &root.work_id
        ));
        assert_eq!(
            engram_root_capture_locked(&inner, &claimed.session_id),
            EngramRootCapture::Unconfirmed
        );
    }
}

#[test]
fn source_root_authority_review_one_total_deadline_covers_prepare_read_and_publish() {
    let label = "authority-total-multiple-phases";
    let claimed = ClaimedRoot::new(label, Vec::new());
    prepare_claimed_root_naming(&claimed, label);
    let store = claimed_root_store(&claimed);
    let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    let target = AppState::engram_binding_target_for_session_shape_locked(
        &claimed.state.inner.lock().unwrap(),
        &claimed.session_id,
        true,
    )
    .unwrap()
    .unwrap();
    let total = target.settings.call_timeout();
    let deadline = std::time::Instant::now() + total;
    let mut state = claimed.state.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    state.persist_tx = tx;
    let worker = state.clone();
    let worker_store = store.clone();
    let worker_binding = binding.clone();
    let task = std::thread::spawn(move || {
        let owner =
            worker.prepare_engram_authority_until(&worker_store, &worker_binding, deadline)?;
        let proof = worker.read_engram_authority_fact_until(&target, &worker_binding, deadline)?;
        AppState::learn_engram_authority_locked(
            &mut worker.inner.lock().unwrap(),
            &worker_store,
            &owner,
            &proof,
        )?;
        worker.publish_engram_authority_until(&worker_store, &owner, deadline)
    });
    let mut batch = PersistFenceBatch::default();
    let mut cache = SqlitePersistConnectionCache::new();
    receive_naming_authority_fence(&rx, &mut batch, &task);
    let prepared = collect_persist_delta_from_shared_state(&state.inner, 0);
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &prepared,
        &mut batch,
    )
    .unwrap();
    receive_naming_authority_fence(&rx, &mut batch, &task);
    // The publication fence shares the ORIGINAL deadline. Deliberately delay
    // only its commit past that deadline; no phase receives a refreshed total.
    std::thread::sleep(
        deadline.saturating_duration_since(std::time::Instant::now()) + Duration::from_millis(5),
    );
    let candidate = collect_persist_delta_from_shared_state(&state.inner, prepared.watermark);
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &candidate,
        &mut batch,
    )
    .unwrap();
    assert!(task.join().unwrap().is_err());
    let inner = state.inner.lock().unwrap();
    assert!(engram_authority_work_unresolved(
        &inner,
        &store,
        &binding.work_id
    ));
    assert_eq!(
        inner.engram_work_naming_history[0]
            .transition
            .as_ref()
            .unwrap()
            .phase,
        EngramAuthorityPhase::Candidate
    );
    assert_eq!(
        engram_root_capture_locked(&inner, &claimed.session_id),
        EngramRootCapture::Unconfirmed
    );
    let reads = claimed
        .transport
        .requests()
        .iter()
        .filter(|request| request.request["operation"] == "named_root_read")
        .count();
    assert_eq!(reads, 1, "no fresh authority I/O after total expiry");
}

#[test]
fn source_root_authority_review_prepared_ack_deadline_explains_no_candidate_fence() {
    let label = "authority-prepared-ack-expiry";
    let claimed = ClaimedRoot::new(label, Vec::new());
    prepare_claimed_root_naming(&claimed, label);
    let store = claimed_root_store(&claimed);
    let binding = claimed.record(|record| record.engram.work_binding.clone().unwrap());
    let target = AppState::engram_binding_target_for_session_shape_locked(
        &claimed.state.inner.lock().unwrap(),
        &claimed.session_id,
        true,
    )
    .unwrap()
    .unwrap();
    let total = target.settings.call_timeout();
    let mut state = claimed.state.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    state.persist_tx = tx;
    let worker = state.clone();
    let worker_store = store.clone();
    let worker_binding = binding.clone();
    let task = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + total;
        let owner = worker
            .prepare_engram_authority_until(&worker_store, &worker_binding, deadline)
            .map_err(|error| ("prepare", error))?;
        let proof = worker
            .read_engram_authority_fact_until(&target, &worker_binding, deadline)
            .map_err(|error| ("read", error))?;
        AppState::learn_engram_authority_locked(
            &mut worker.inner.lock().unwrap(),
            &worker_store,
            &owner,
            &proof,
        )
        .map_err(|error| ("learn", error))?;
        worker
            .publish_engram_authority_until(&worker_store, &owner, deadline)
            .map_err(|error| ("publish", error))
    });
    let mut batch = PersistFenceBatch::default();
    receive_naming_authority_fence(&rx, &mut batch, &task);
    // Hold exactly the first acknowledgement until its existing total expires.
    // An absent second fence is then an identified Prepared deadline, not a
    // claim that the Candidate owner or canonical read was malformed.
    let (phase, error) = task
        .join()
        .unwrap()
        .expect_err("no Prepared acknowledgement before the deadline");
    eprintln!("forced first acknowledgement outcome: phase={phase} error={error:?}");
    assert_eq!(phase, "prepare");
    assert!(error.message.contains("(Deadline)"), "{error:?}");
    assert!(
        !rx.try_iter()
            .any(|request| matches!(request, PersistRequest::Fence(_)))
    );
    assert!(
        !claimed
            .transport
            .requests()
            .iter()
            .any(|request| request.request["operation"] == "named_root_read")
    );
    assert!(
        state.inner.inner.try_lock().is_ok(),
        "waiting never held StateInner"
    );
    let prepared = collect_persist_delta_from_shared_state(&state.inner, 0);
    let mut cache = SqlitePersistConnectionCache::new();
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &prepared,
        &mut batch,
    )
    .unwrap();
    let restored = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(engram_authority_work_unresolved(
        &restored,
        &store,
        &binding.work_id
    ));
    assert_eq!(
        restored.engram_work_naming_history[0]
            .transition
            .as_ref()
            .unwrap()
            .phase,
        EngramAuthorityPhase::Prepared
    );
}

#[test]
fn source_root_opening_notice_recomposition_and_acknowledgement_keep_exact_owners() {
    let claimed = ClaimedRoot::new_scripted(
        "opening-notice-owner",
        vec![
            bind_reply("notice-owner-token"),
            grant_reply("notice-owner-grant"),
            begin_reply("notice-owner-grant"),
            checkpoint_reply("notice-owner-grant"),
        ],
    );
    prepare_confirmed_claimed_opening(&claimed, "opening-notice-owner");
    claimed.record(|record| {
        record
            .engram
            .set_pending_source_root_line("[TermAl] ordinary naming line".to_owned())
    });
    let mut dispatch = claimed.dispatch();
    let generation = dispatch.active_turn_generation();
    let notice = EngramOpeningDiagnostic {
        id: "first-diagnostic".to_owned(),
        turn_generation: generation,
        grant_id: "first-grant".to_owned(),
        reason: EngramOpeningRootReason::MissingProjection,
        delivered: false,
    };
    // Identical notice prose is also legitimate user text. Only the host's
    // composition slot may be removed, never that user text or naming lines.
    let TurnDispatch::PersistentCodex { command, .. } = &mut dispatch else {
        panic!("Codex fixture");
    };
    command
        .prompt
        .push_str(&format!("\nUser quotation: {}", notice.reason.notice()));
    let base = command.prompt.clone();
    claimed.record(|record| {
        record.engram.active_grant_id = Some(notice.grant_id.clone());
        record.engram.opening_diagnostic = Some(notice.clone());
        assert_eq!(
            refresh_engram_source_root_prompt_locked(record, &mut dispatch),
            Some(notice.clone())
        );
        assert_eq!(
            refresh_engram_source_root_prompt_locked(record, &mut dispatch),
            Some(notice.clone())
        );
    });
    let TurnDispatch::PersistentCodex { command, .. } = &dispatch else {
        panic!("Codex fixture");
    };
    assert_eq!(
        command.prompt,
        format!("{}\n\n{base}", notice.reason.notice())
    );
    let successor = EngramOpeningDiagnostic {
        id: "replacement-diagnostic".to_owned(),
        ..notice.clone()
    };
    claimed.record(|record| {
        record.engram.opening_diagnostic = Some(successor.clone());
        acknowledge_engram_opening_diagnostic_locked(record, &notice);
        assert_eq!(
            record.engram.opening_diagnostic,
            Some(successor.clone()),
            "equal prose is not equal delivery identity"
        );
        acknowledge_engram_opening_diagnostic_locked(record, &successor);
        assert!(record.engram.opening_diagnostic.as_ref().unwrap().delivered);
        assert!(refresh_engram_source_root_prompt_locked(record, &mut dispatch).is_none());
    });
    let TurnDispatch::PersistentCodex { command, .. } = &dispatch else {
        panic!("Codex fixture");
    };
    assert_eq!(
        command.prompt, base,
        "recomposition preserves the user quotation and ordinary naming line"
    );
    claimed.record(|record| {
        let next = EngramOpeningDiagnostic {
            id: "next-opening".to_owned(),
            turn_generation: generation + 1,
            grant_id: "next-grant".to_owned(),
            ..notice.clone()
        };
        record.active_turn_generation += 1;
        record.engram.active_grant_id = Some(next.grant_id.clone());
        record.engram.opening_diagnostic = Some(next.clone());
        acknowledge_engram_opening_diagnostic_locked(record, &successor);
        assert!(refresh_engram_source_root_prompt_locked(record, &mut dispatch).is_none());
        assert_eq!(record.engram.opening_diagnostic, Some(next));
        assert!(
            record
                .engram
                .pending_source_root_line
                .as_deref()
                .unwrap()
                .contains("ordinary naming line")
        );
    });
}

#[test]
fn source_root_opening_notice_failed_provider_send_is_not_delivery() {
    let label = "opening-notice-send-failed";
    let grant = "opening-notice-send-failed-grant";
    let claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply("notice-token"),
            grant_reply(grant),
            begin_reply(grant),
            checkpoint_reply(grant),
        ],
    );
    let dispatch = claimed.dispatch();
    let state = claimed.state.clone();
    let session = claimed.session_id.clone();
    drop(claimed.runtime_rx);
    assert!(deliver_turn_dispatch(&state, dispatch).is_err());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    let notice = record
        .engram
        .opening_diagnostic
        .as_ref()
        .expect("failed send retains the original opening diagnostic");
    assert_eq!(notice.grant_id, grant);
    assert!(!notice.delivered);
    assert_eq!(notice.reason, EngramOpeningRootReason::UnverifiedStore);
    assert_eq!(
        record.active_turn_generation, notice.turn_generation,
        "no notice-triggered replacement turn"
    );
}
