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
    assert_eq!(
        basis["source_revision"].as_str(),
        Some(content_revision_of(FsPath::new(root)).as_str())
    );
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
fn a_focus_change_refuses_one_close_then_captures_the_next_quiet_turn() {
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
fn a_claim_taken_mid_turn_changes_only_the_next_opening() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let label = "focus-taken-mid-turn";
    let (turn, first_path) = confirmation_root(label);
    turn.state
        .install_control_test_transport(FocusSelectingTransport::new(turn.transport.clone()));
    let mut held = naming_claim(label);
    let first_binding = held.items[0].control_binding.clone().unwrap();
    request_naming(&turn, label, Some(&first_path), vec![held.clone()]).unwrap();
    turn.begin("Keep this opening when a new claim is taken later.");
    let opening = turn.record(|record| record.engram.observed_opening().cloned().unwrap());
    let turn_ref = observed_turn_ref(&turn);
    let second_binding = test_control_work_binding("new-mid-turn-claim", 1);
    held.items.push(second_claim(&second_binding));
    // The producer fixture admits observations for one session association.
    // Model the newly held claim there too; registering its run alone would
    // leave the producer rejecting B's observations under the old A scope.
    // This changes no host cache and emits no root naming event.
    turn.transport
        .enable_named_roots(&turn.session_id, &second_binding);
    focus(&turn, &held, &second_binding);
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
                == format!("termal-source-sighting:{turn_ref}")),
        "a changed close cannot relabel the earlier root as the new claim"
    );
    turn.record(|record| {
        assert!(
            record
                .engram
                .unadmitted_closing_jobs
                .iter()
                .any(|job| job.trace.host_turn_ref == turn_ref && job.terminal_failure.is_some()),
            "the immutable old closing scope stays refused"
        )
    });

    let prompt = turn
        .begin_prompt("The next ordinary turn belongs to the newly held claim.")
        .prompt;
    assert!(prompt.contains("not captured because focus moved"));
    assert!(prompt.contains("refreshed for a later opening"));
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
