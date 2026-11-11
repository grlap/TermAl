//! Focus changes use the real held-claim selector, independently of naming's
//! cached association. No source command or second naming request drives a turn.
use super::*;

#[path = "engram_source_root_focus_refusals.rs"]
mod refusals;

struct FocusSelectingTransport {
    inner: Arc<ScriptedEngramControlTransport>,
    root_read_hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl FocusSelectingTransport {
    fn new(inner: Arc<ScriptedEngramControlTransport>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            root_read_hook: Mutex::new(None),
        })
    }
}

impl EngramControlTransport for FocusSelectingTransport {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        if matches!(request, EngramControlRequest::NamedRootRead { .. }) {
            let hook = self.root_read_hook.lock().unwrap().take();
            if let Some(hook) = hook {
                hook();
            }
        }
        self.inner.request(connection, request, timeout)
    }

    fn read_observation_policy(
        &self,
        connection: &EngramConnectionConfig,
        store: &EngramAuthorityStoreKey,
        timeout: Duration,
        cwd: &FsPath,
    ) -> Result<EngramObservationPolicyBasis, EngramTransportError> {
        self.inner
            .read_observation_policy(connection, store, timeout, cwd)
    }

    fn read_held_claims(
        &self,
        connection: &EngramConnectionConfig,
        timeout: Duration,
    ) -> Result<EngramHeldClaims, EngramTransportError> {
        self.inner.read_held_claims(connection, timeout)
    }

    fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        let held = self.read_held_claims(connection, timeout)?;
        Ok(select_engram_held_binding(
            held.items,
            held.omitted,
            preference,
        ))
    }

    fn shutdown_session(&self, session: &str) {
        self.inner.shutdown_session(session);
    }
}

fn focus(turn: &EOCheckedTurn, held: &EngramHeldClaims, selected: &EngramControlWorkBinding) {
    let mut held = held.clone();
    for claim in &mut held.items {
        claim.focused = claim.claim_id == selected.claim_id;
    }
    turn.transport.replace_held_claims([Ok(held)]);
}

fn second_claim(binding: &EngramControlWorkBinding) -> EngramHeldClaim {
    EngramHeldClaim {
        work_id: binding.work_id.clone(),
        short_ref: "w-second".to_owned(),
        claim_id: binding.claim_id.clone(),
        claim_fence: binding.claim_fence,
        focused: false,
        control_binding: Some(binding.clone()),
    }
}

fn settle_close(turn: &EOCheckedTurn) {
    let guard = phase_sync::PollGuard::new();
    loop {
        if turn.record(|record| {
            !record.engram.unadmitted_worker_running
                && record.engram.unadmitted_active_turn.is_none()
                && record
                    .engram
                    .unadmitted_closing_jobs
                    .iter()
                    .all(|job| job.terminal_failure.is_some())
                && record
                    .engram
                    .unadmitted_reports
                    .iter()
                    .all(|report| report.is_settled())
        }) {
            return;
        }
        guard.wait("the ordinary closing owner records or refuses the exact turn");
    }
}

fn assert_named_sighting(
    turn: &EOCheckedTurn,
    turn_ref: &str,
    binding: &EngramControlWorkBinding,
    root: &str,
    generation: u64,
) {
    let key = format!("termal-source-sighting:{turn_ref}");
    let requests = turn.transport.requests();
    let matching = requests
        .iter()
        .filter(|sent| sent.request["idempotency_key"] == key)
        .collect::<Vec<_>>();
    let failures = turn.record(|record| {
        record
            .engram
            .unadmitted_closing_jobs
            .iter()
            .filter(|job| job.trace.host_turn_ref == turn_ref)
            .filter_map(|job| job.terminal_failure.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        matching.len(),
        1,
        "the focused root needs its own measured sighting; failures: {failures:?}"
    );
    let request = &matching[0].request;
    assert_eq!(request["binding"], json!(binding));
    let basis = &request["occurrence"]["source_basis"];
    assert_eq!(basis["workspace_id"].as_str(), Some(root));
    assert_eq!(basis["source_root_generation"], generation);
    assert_eq!(basis["source_root_state"], "named");
    assert_eq!(basis["source_id"], owner_source_of(turn, FsPath::new(root)));
    assert!(request.get("verification_evidence").is_none());
    assert_recorded_turn(turn, turn_ref);
}

fn assert_recorded_turn(turn: &EOCheckedTurn, turn_ref: &str) {
    turn.record(|record| {
        let report = record
            .engram
            .unadmitted_reports
            .iter()
            .find(|report| report.host_turn_ref == turn_ref)
            .expect("the closing report is retained");
        assert!(
            report.envelopes.iter().all(|envelope| matches!(
                envelope.delivery,
                EngramRuntimeObservationDelivery::Recorded { .. }
            )),
            "the producer acknowledged this turn's observations"
        );
    });
}

fn assert_private_focus_notice(text: &str, bindings: &[&EngramControlWorkBinding]) {
    assert!(!text.contains("\"source_id\"") && !text.contains("\"source_basis\""));
    for binding in bindings {
        assert!(
            !text.contains(&binding.work_id) && !text.contains(&binding.claim_id),
            "the notice must not copy raw canonical work or claim IDs"
        );
    }
}

fn assert_private_focus_system_notices(
    record: &SessionRecord,
    bindings: &[&EngramControlWorkBinding],
) {
    for message in &record.session.messages {
        if let Message::Text {
            author: Author::System,
            text,
            ..
        } = message
        {
            assert_private_focus_notice(text, bindings);
        }
    }
}

#[test]
fn a_forward_work_revision_refreshes_only_the_next_quiet_turn() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let label = "forward-work-revision";
    let (turn, path) = confirmation_root(label);
    turn.state
        .install_control_test_transport(FocusSelectingTransport::new(turn.transport.clone()));
    let mut held = naming_claim(label);
    let first = held.items[0].control_binding.clone().unwrap();
    let named = request_naming(&turn, label, Some(&path), vec![held.clone()]).unwrap();
    let names_before = turn
        .state
        .inner
        .lock()
        .unwrap()
        .engram_work_source_roots
        .clone();

    turn.begin("Keep the opening captured before the work criteria change.");
    let opening = turn.record(|record| record.engram.observed_opening().cloned().unwrap());
    let previous_ref = observed_turn_ref(&turn);
    let mut revised = first.clone();
    revised.work_revision = 2;
    held.items[0].control_binding = Some(revised.clone());
    // The producer's current claim view changes without a new run or root
    // event. Neither this update nor the held-claim read changes host state.
    turn.transport.register_named_root_run(&revised);
    focus(&turn, &held, &revised);
    assert_eq!(
        turn.record(|record| record.engram.observed_opening().cloned()),
        Some(opening)
    );
    turn.end();
    settle_close(&turn);
    assert!(
        !turn
            .transport
            .requests()
            .iter()
            .any(|sent| sent.request["idempotency_key"]
                == format!("termal-source-sighting:{previous_ref}")),
        "the revised criteria cannot relabel the earlier opening"
    );
    turn.record(|record| {
        assert!(
            record
                .engram
                .unadmitted_closing_jobs
                .iter()
                .any(|job| job.trace.host_turn_ref == previous_ref
                    && job.terminal_failure.is_some()
                    && job.scope.binding.as_ref() == Some(&first)),
            "the old closing job must retain its original revision"
        );
    });

    turn.begin("Measure the existing named root under the current work revision.");
    let next_ref = observed_turn_ref(&turn);
    turn.end();
    settle_close(&turn);
    assert_named_sighting(
        &turn,
        &next_ref,
        &revised,
        named.root.as_ref().unwrap(),
        named.generation,
    );
    assert_eq!(
        turn.state.inner.lock().unwrap().engram_work_source_roots,
        names_before,
        "a revision refresh cannot create or rewrite a name"
    );
}

#[test]
fn a_revision_advance_keeps_changed_claims_and_roots_unconfirmed() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for change in [
        "backwards",
        "claim",
        "fence",
        "run",
        "execution",
        "root",
        "authority",
    ] {
        let label = format!("revision-refuses-{change}");
        let (turn, path) = confirmation_root(&label);
        turn.state
            .install_control_test_transport(FocusSelectingTransport::new(turn.transport.clone()));
        let mut held = naming_claim(&label);
        let mut first = held.items[0].control_binding.clone().unwrap();
        if change == "backwards" {
            first.work_revision = 2;
        }
        held.items[0].control_binding = Some(first.clone());
        turn.transport.register_named_root_run(&first);
        request_naming(&turn, &label, Some(&path), vec![held.clone()]).unwrap();
        let names_before = turn
            .state
            .inner
            .lock()
            .unwrap()
            .engram_work_source_roots
            .clone();
        turn.begin("Do not adopt a changed claim or root as a revision refresh.");
        let previous_ref = observed_turn_ref(&turn);
        let mut changed = first.clone();
        changed.work_revision = if change == "backwards" { 1 } else { 2 };
        match change {
            "claim" => changed.claim_id.push_str("-different"),
            "fence" => changed.claim_fence += 1,
            "run" => changed.run_id.push_str("-different"),
            "execution" => changed.root_execution_id.push_str("-different"),
            _ => {}
        }
        held.items[0].claim_id = changed.claim_id.clone();
        held.items[0].claim_fence = changed.claim_fence;
        held.items[0].control_binding = Some(changed.clone());
        turn.transport.register_named_root_run(&changed);
        if change == "root" {
            let mut roots = turn.transport.named_roots.lock().unwrap();
            let roots = roots.as_mut().unwrap();
            let mut proof = roots
                .request(
                    &turn.session_id,
                    &EngramControlRequest::NamedRootRead {
                        run_id: changed.run_id.clone(),
                        claim_id: changed.claim_id.clone(),
                    },
                )
                .unwrap()
                .unwrap();
            let generation = proof["named_root"]["generation"].as_i64().unwrap() + 1;
            let position = proof["read_cut"]["position"].as_i64().unwrap() + 1;
            proof["named_root"]["generation"] = json!(generation);
            proof["latest_event"]["generation"] = json!(generation);
            proof["latest_event"]["event"] = json!(sha256_hex(b"a newer root event"));
            proof["latest_event"]["position"]["position"] = json!(position);
            proof["read_cut"]["position"] = json!(position);
            roots.root_reads.insert(changed.claim_id.clone(), proof);
        }
        focus(&turn, &held, &changed);
        if change == "authority" {
            let store = turn
                .state
                .inner
                .lock()
                .unwrap()
                .engram_work_source_roots
                .iter()
                .find(|root| root.work_id == first.work_id)
                .unwrap()
                .store
                .clone();
            let clock = turn.state.engram_budget_clock();
            turn.state
                .prepare_engram_authority_until(&store, &first, clock.now() + DEADLOCK_GUARD)
                .unwrap();
        }
        turn.end();
        settle_close(&turn);
        turn.begin("A later turn must still refuse the mismatched association.");
        let next_ref = observed_turn_ref(&turn);
        turn.end();
        settle_close(&turn);
        for turn_ref in [&previous_ref, &next_ref] {
            assert!(
                !turn
                    .transport
                    .requests()
                    .iter()
                    .any(|sent| sent.request["idempotency_key"]
                        == format!("termal-source-sighting:{turn_ref}")),
                "{change}"
            );
        }
        assert_eq!(
            turn.record(|record| record.engram.work_binding.clone()),
            Some(first),
            "{change}"
        );
        assert_eq!(
            turn.state.inner.lock().unwrap().engram_work_source_roots,
            names_before,
            "{change}"
        );
    }
}

#[test]
fn switching_between_held_named_items_keeps_the_previous_turns_scope() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let label = "held-named-focus-switch";
    let (turn, first_path) = confirmation_root(label);
    let transport = FocusSelectingTransport::new(turn.transport.clone());
    turn.state.install_control_test_transport(transport.clone());
    let mut held = naming_claim(label);
    let first_binding = held.items[0].control_binding.clone().unwrap();
    let second_binding = test_control_work_binding("held-named-second", 1);
    held.items.push(second_claim(&second_binding));
    let first = request_naming(&turn, label, Some(&first_path), vec![held.clone()]).unwrap();
    let second_path = turn.root.join(".worktrees").join("held-second");
    run_git_test_command(
        &turn.root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "held-second",
            second_path.to_str().unwrap(),
        ],
    );
    turn.transport.register_named_root_run(&second_binding);
    focus(&turn, &held, &second_binding);
    let second = turn
        .state
        .name_engram_source_root(
            &turn.session_id,
            EngramSourceRootRequest {
                work: "w-second".to_owned(),
                path: Some(Some(second_path.to_string_lossy().into_owned())),
            },
        )
        .unwrap();
    // Naming B registers its root but this producer fixture still accepts
    // only A's observations until its explicit session association moves.
    // Model B here, then move back to A only after B's report settles below.
    turn.transport
        .enable_named_roots(&turn.session_id, &second_binding);
    let names_before = turn
        .state
        .inner
        .lock()
        .unwrap()
        .engram_work_source_roots
        .clone();
    turn.begin("Finish the turn under the second named item before moving on.");
    let previous_ref = observed_turn_ref(&turn);
    assert_eq!(
        turn.record(|record| record.engram.observed_opening().unwrap().binding.clone()),
        second_binding,
        "the opening is already attributed before the focus switch"
    );
    let saw_retained_report = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let saw_report = saw_retained_report.clone();
    let state = turn.state.clone();
    let session = turn.session_id.clone();
    let closed = previous_ref.clone();
    let weak_transport = Arc::downgrade(&transport);
    // The first read validates the original root. The second reads the next
    // root, and must already see the old report retained in the session row.
    *transport.root_read_hook.lock().unwrap() = Some(Box::new(move || {
        let transport = weak_transport.upgrade().unwrap();
        *transport.root_read_hook.lock().unwrap() = Some(Box::new(move || {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(
                record
                    .engram
                    .unadmitted_reports
                    .iter()
                    .any(|report| report.host_turn_ref == closed)
            );
            assert!(
                !record
                    .engram
                    .unadmitted_closing_jobs
                    .iter()
                    .any(|job| job.trace.host_turn_ref == closed)
            );
            saw_report.store(true, std::sync::atomic::Ordering::SeqCst);
        }));
    }));
    focus(&turn, &held, &first_binding);
    turn.end();
    settle_close(&turn);

    assert_named_sighting(
        &turn,
        &previous_ref,
        &second_binding,
        second.root.as_ref().unwrap(),
        second.generation,
    );
    assert!(
        saw_retained_report.load(std::sync::atomic::Ordering::SeqCst),
        "the successor proof read follows installation of the original report"
    );
    turn.record(|record| {
        assert!(
            !record
                .engram
                .unadmitted_closing_jobs
                .iter()
                .any(
                    |job| job.trace.host_turn_ref == previous_ref && job.terminal_failure.is_some()
                ),
            "an ordinary held-item switch must not terminalize the earlier turn"
        )
    });

    // This fixture models one producer association at a time; move it only
    // after the earlier report has been acknowledged, without changing names.
    turn.transport
        .enable_named_roots(&turn.session_id, &first_binding);
    let prompt = turn
        .begin_prompt("Continue under the first already named item.")
        .prompt;
    assert!(!prompt.contains("runtime_observation_scope_changed"));
    assert!(!prompt.contains("not captured because focus moved"));
    let next_ref = observed_turn_ref(&turn);
    turn.end();
    settle_close(&turn);
    assert_named_sighting(
        &turn,
        &next_ref,
        &first_binding,
        first.root.as_ref().unwrap(),
        first.generation,
    );
    assert_eq!(
        turn.state.inner.lock().unwrap().engram_work_source_roots,
        names_before,
        "switching focus must neither rename nor rewrite either source root"
    );
}

#[test]
fn a_released_claim_refuses_one_close_then_captures_the_next_quiet_turn() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let label = "focus-before-ordinary-opening";
    let (turn, first_path) = confirmation_root(label);
    turn.state
        .install_control_test_transport(FocusSelectingTransport::new(turn.transport.clone()));
    let mut held = naming_claim(label);
    let first_binding = held.items[0].control_binding.clone().unwrap();
    let second_binding = test_control_work_binding("focus-second-claim", 1);
    held.items.push(second_claim(&second_binding));
    let first = request_naming(&turn, label, Some(&first_path), vec![held.clone()]).unwrap();
    let second_path = turn.root.join(".worktrees").join("second");
    run_git_test_command(
        &turn.root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "second",
            second_path.to_str().unwrap(),
        ],
    );
    turn.transport.register_named_root_run(&second_binding);
    focus(&turn, &held, &second_binding);
    let second = turn
        .state
        .name_engram_source_root(
            &turn.session_id,
            EngramSourceRootRequest {
                work: "w-second".to_owned(),
                path: Some(Some(second_path.to_string_lossy().into_owned())),
            },
        )
        .unwrap();
    let names_before = turn
        .state
        .inner
        .lock()
        .unwrap()
        .engram_work_source_roots
        .clone();
    // The previous claim is no longer held. Unlike a focus-only switch,
    // this must retain the old turn's scope-change refusal.
    held.items
        .retain(|claim| claim.claim_id != second_binding.claim_id);
    focus(&turn, &held, &first_binding);
    turn.begin("Observe the focused root without running a command or re-naming it.");
    let turn_ref = observed_turn_ref(&turn);
    turn.end();
    settle_close(&turn);
    assert!(
        !turn
            .transport
            .requests()
            .iter()
            .any(|sent| sent.request["idempotency_key"]
                == format!("termal-source-sighting:{turn_ref}")),
        "the first mismatched opening must stay uncaptured"
    );
    turn.record(|record| {
        assert!(
            record
                .engram
                .unadmitted_closing_jobs
                .iter()
                .any(|job| job.trace.host_turn_ref == turn_ref
                    && job.terminal_failure.is_some()
                    && job.scope.binding.as_ref() == Some(&second_binding)),
            "refresh must not rewrite the refused closing job"
        )
    });
    let prompt = turn
        .begin_prompt("Capture the already named focused root on a later turn.")
        .prompt;
    assert!(prompt.contains("not captured because focus moved"));
    assert!(prompt.contains("refreshed for a later opening"));
    assert_private_focus_notice(&prompt, &[&first_binding, &second_binding]);
    turn.record(|record| {
        assert_private_focus_system_notices(record, &[&first_binding, &second_binding])
    });
    let next_ref = observed_turn_ref(&turn);
    turn.end();
    settle_close(&turn);
    assert_named_sighting(
        &turn,
        &next_ref,
        &first_binding,
        first.root.as_ref().unwrap(),
        first.generation,
    );
    assert_ne!(first.generation, second.generation);
    assert_eq!(
        turn.state.inner.lock().unwrap().engram_work_source_roots,
        names_before,
        "focus cannot create or rewrite a name"
    );
}

#[test]
fn a_claim_taken_mid_turn_keeps_the_original_report_and_changes_the_next_opening() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let label = "focus-taken-mid-turn";
    let (turn, first_path) = confirmation_root(label);
    turn.state
        .install_control_test_transport(FocusSelectingTransport::new(turn.transport.clone()));
    let mut held = naming_claim(label);
    let first_binding = held.items[0].control_binding.clone().unwrap();
    let first = request_naming(&turn, label, Some(&first_path), vec![held.clone()]).unwrap();
    turn.begin("Keep this opening when a new claim is taken later.");
    let opening = turn.record(|record| record.engram.observed_opening().cloned().unwrap());
    let turn_ref = observed_turn_ref(&turn);
    let second_binding = test_control_work_binding("new-mid-turn-claim", 1);
    held.items.push(second_claim(&second_binding));
    turn.transport.register_named_root_run(&second_binding);
    focus(&turn, &held, &second_binding);
    assert_eq!(
        turn.record(|record| record.engram.observed_opening().cloned()),
        Some(opening)
    );
    turn.end();
    settle_close(&turn);
    assert_named_sighting(
        &turn,
        &turn_ref,
        &first_binding,
        first.root.as_ref().unwrap(),
        first.generation,
    );

    // Move the fixture's one producer association after A's report settles.
    turn.transport
        .enable_named_roots(&turn.session_id, &second_binding);

    let prompt = turn
        .begin_prompt("The next ordinary turn belongs to the newly held claim.")
        .prompt;
    assert!(!prompt.contains("runtime_observation_scope_changed"));
    assert!(!prompt.contains("not captured because focus moved"));
    assert_private_focus_notice(&prompt, &[&first_binding, &second_binding]);
    let next_ref = observed_turn_ref(&turn);
    turn.end();
    settle_close(&turn);
    let requests = turn.transport.requests();
    let key = format!("termal-source-sighting:{next_ref}");
    let sighting = requests
        .iter()
        .find(|sent| sent.request["idempotency_key"] == key)
        .expect(
            "the later quiet turn must capture under the new claim without re-naming the old root",
        );
    assert_eq!(sighting.request["binding"], json!(second_binding));
    assert_eq!(
        sighting.request["occurrence"]["source_basis"]["workspace_id"].as_str(),
        Some(fs::canonicalize(&turn.root).unwrap().to_str().unwrap())
    );
    assert!(
        sighting.request["occurrence"]["source_basis"]
            .get("source_root_generation")
            .is_none(),
        "the new claim did not inherit the first claim's named generation"
    );
    assert_recorded_turn(&turn, &next_ref);
    assert_ne!(first_binding.claim_id, second_binding.claim_id);
}
