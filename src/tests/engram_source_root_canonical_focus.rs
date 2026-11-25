//! Canonical focus and a successful prospective name do not relabel the
//! active turn, or let its controlled successor overtake its closing report.
use super::*;

#[test]
fn restart_recovers_the_acknowledged_named_root_for_a_later_ordinary_turn() {
    let label = "restart-named-opening";
    let before = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&before.root);
    let mut held = naming_claim(label);
    let claim = &held.items[0];
    let claim_id = claim.claim_id.clone();
    let new_fence = claim.claim_fence + 1;
    before
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .claim_fences
        .insert(claim_id.clone(), claim.claim_fence);
    let named = request_naming(&before, label, Some(&worktree), vec![held.clone()]).unwrap();
    before.begin("Report the original named source.");
    before.finish();
    let source_owner = before
        .state
        .inner
        .lock()
        .unwrap()
        .test_observation_source_owner
        .clone();
    let mut new_source = before.sources.get(&worktree);
    new_source.commit = "c".repeat(40);
    new_source.tree = "d".repeat(40);
    before.set_source_id(&worktree, new_source.clone());

    before.state.trigger_shutdown_signal();
    before.state.shutdown_persist_blocking();
    let root = before.state.test_temp_root.as_ref().unwrap().clone();
    let restarted = boot_on_test_temp_root(&root, || {
        AppState::new_with_paths_engram_transport_and_clock_for_test(
            before.root.to_string_lossy().into_owned(),
            before.state.persistence_path.as_ref().clone(),
            before.state.orchestrator_templates_path.as_ref().clone(),
            before.transport.clone(),
            EngramBudgetClock::scripted(),
        )
        .unwrap()
    });
    restarted
        .inner
        .lock()
        .unwrap()
        .test_observation_source_owner = source_owner;
    let (runtime, commands, _process) = test_shared_codex_runtime(label);
    install_single_wire_codex_fixture(&restarted, Some(runtime));
    let after = EOCheckedTurn::from_session(
        restarted,
        before.session_id.clone(),
        before.root.clone(),
        before.transport.clone(),
        Arc::new(Mutex::new(commands)),
        before.sources.clone(),
    );
    after.begin("First ordinary turn after restart.");
    let producer = after.transport.clone();
    let authority = Arc::new(CanonicalFocusTransport {
        inner: after.transport.clone(),
        closing_read: Mutex::new(None),
        before_named_root_read: Mutex::new(Some(Box::new(move || {
            producer
                .named_roots
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .claim_fences
                .insert(claim_id, new_fence);
        }))),
        handoff_thread: std::thread::current().id(),
        handoff_reads: std::sync::atomic::AtomicUsize::new(0),
    });
    after
        .state
        .install_control_test_transport(authority.clone());
    finish_unbound_restart_turn(&after);
    assert!(authority.before_named_root_read.lock().unwrap().is_none());
    assert!(
        after.record(|record| record.engram.work_binding.is_none()),
        "the later named-root proof must prevent the earlier fence from being installed"
    );

    let claim = &mut held.items[0];
    claim.claim_fence = new_fence;
    let binding = claim.control_binding.as_mut().unwrap();
    binding.claim_fence = new_fence;
    after.transport.register_named_root_run(binding);
    after
        .transport
        .set_current_work_binding(Some(binding.clone()));
    after.transport.replace_held_claims([Ok(held)]);
    after.begin("Recover the held claim under its current fence.");
    finish_unbound_restart_turn(&after);

    after.begin("Report the source from a fresh opening.");
    let later = after.record(|record| {
        let trace = record.engram.unadmitted_active_turn.as_ref().unwrap();
        assert_eq!(
            trace
                .opening_root
                .as_ref()
                .and_then(|opening| opening.source_root.as_ref())
                .map(|root| root.generation),
            Some(named.generation),
            "a consistent live proof restores the named association for the next opening"
        );
        assert_eq!(
            trace.opening_root.as_ref().unwrap().binding.claim_fence,
            new_fence
        );
        trace.host_turn_ref.clone()
    });
    after.end();
    let guard = phase_sync::PollGuard::new();
    let recorded = loop {
        let (failure, envelopes) = after.record(|record| {
            let failure = record
                .engram
                .unadmitted_closing_jobs
                .iter()
                .find(|job| job.trace.host_turn_ref == later)
                .and_then(|job| job.terminal_failure.clone());
            let envelopes = record
                .engram
                .unadmitted_reports
                .iter()
                .find(|report| report.host_turn_ref == later && report.is_settled())
                .map(|report| report.envelopes.clone());
            (failure, envelopes)
        });
        assert!(failure.is_none(), "the recovered close failed: {failure:?}");
        if let Some(envelopes) = envelopes {
            break envelopes;
        }
        guard.wait("the recovered close settles its observation");
    };
    let sighting = recorded
        .iter()
        .find(|envelope| envelope.request["occurrence"]["kind"] == "source_sighting")
        .expect("the recovered close must include a measured source sighting");
    let EngramRuntimeObservationDelivery::Recorded { receipt } = &sighting.delivery else {
        panic!(
            "the source sighting did not reach the producer: {:?}",
            sighting.delivery
        );
    };
    assert_eq!(receipt["accounting"]["kind"], "sighting");
    let basis = &sighting.request["occurrence"]["source_basis"];
    assert_eq!(basis["workspace_id"], json!(named.root.unwrap()));
    assert_eq!(basis["source_root_generation"], json!(named.generation));
    assert_eq!(basis["source_root_state"], "named");
    assert_eq!(basis["source_id"], json!(new_source));
    after.state.trigger_shutdown_signal();
    after.state.shutdown_persist_blocking();
}

fn finish_unbound_restart_turn(turn: &EOCheckedTurn) {
    let unbound = turn.record(|record| {
        let trace = record.engram.unadmitted_active_turn.as_ref().unwrap();
        assert!(trace.opening_root.is_none());
        trace.host_turn_ref.clone()
    });
    turn.end();
    let guard = phase_sync::PollGuard::new();
    while !turn.record(|record| {
        record
            .engram
            .unadmitted_closing_jobs
            .iter()
            .any(|job| job.trace.host_turn_ref == unbound && job.terminal_failure.is_some())
    }) {
        guard.wait("the unbound restart turn is refused");
    }
    assert!(!turn.record(|record| {
        record
            .engram
            .unadmitted_reports
            .iter()
            .any(|report| report.host_turn_ref == unbound)
    }));
}

struct CanonicalFocusTransport {
    inner: Arc<ScriptedEngramControlTransport>,
    closing_read: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    before_named_root_read: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    handoff_thread: std::thread::ThreadId,
    handoff_reads: std::sync::atomic::AtomicUsize,
}

pub(super) fn install_canonical_focus_reader(turn: &EOCheckedTurn) {
    turn.state
        .install_control_test_transport(Arc::new(CanonicalFocusTransport {
            inner: turn.transport.clone(),
            closing_read: Mutex::new(None),
            before_named_root_read: Mutex::new(None),
            handoff_thread: std::thread::current().id(),
            handoff_reads: std::sync::atomic::AtomicUsize::new(0),
        }));
}

impl CanonicalFocusTransport {
    fn count_read(&self) {
        if std::thread::current().id() == self.handoff_thread {
            self.handoff_reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn handoff_reads(&self) -> usize {
        self.handoff_reads.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl EngramControlTransport for CanonicalFocusTransport {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        self.count_read();
        if matches!(request, EngramControlRequest::NamedRootRead { .. })
            && let Some(before_read) = self.before_named_root_read.lock().unwrap().take()
        {
            before_read();
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
        self.count_read();
        self.inner
            .read_observation_policy(connection, store, timeout, cwd)
    }

    fn read_held_claims(
        &self,
        connection: &EngramConnectionConfig,
        timeout: Duration,
    ) -> Result<EngramHeldClaims, EngramTransportError> {
        self.count_read();
        self.inner.read_held_claims(connection, timeout)
    }

    fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.count_read();
        let hook = self.closing_read.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
        let held = self.inner.read_held_claims(connection, timeout)?;
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

struct CanonicalFocusFixture {
    turn: EOCheckedTurn,
    transport: Arc<CanonicalFocusTransport>,
    held: EngramHeldClaims,
    a: EngramControlWorkBinding,
    b: EngramControlWorkBinding,
    a_name: EngramSourceRootResponse,
    b_path: PathBuf,
}

impl CanonicalFocusFixture {
    fn new(label: &str) -> Self {
        let turn = EOCheckedTurn::idle(label, true);
        let a_path = naming_worktree(&turn.root);
        let transport = Arc::new(CanonicalFocusTransport {
            inner: turn.transport.clone(),
            closing_read: Mutex::new(None),
            before_named_root_read: Mutex::new(None),
            handoff_thread: std::thread::current().id(),
            handoff_reads: std::sync::atomic::AtomicUsize::new(0),
        });
        turn.state.install_control_test_transport(transport.clone());
        let mut held = naming_claim(label);
        let a = held.items[0].control_binding.clone().unwrap();
        let b = test_control_work_binding(&format!("{label}-next"), 1);
        held.items.push(EngramHeldClaim {
            work_id: b.work_id.clone(),
            short_ref: "w-next".to_owned(),
            claim_id: b.claim_id.clone(),
            claim_fence: b.claim_fence,
            focused: false,
            control_binding: Some(b.clone()),
        });
        let a_name = request_naming(&turn, label, Some(&a_path), vec![held.clone()]).unwrap();
        let b_path = turn.root.join(".worktrees").join("next");
        run_git_test_command(
            &turn.root,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "next",
                b_path.to_str().unwrap(),
            ],
        );
        turn.transport.register_named_root_run(&b);
        Self {
            turn,
            transport,
            held,
            a,
            b,
            a_name,
            b_path,
        }
    }

    fn name_focused_b(&self) -> EngramSourceRootResponse {
        let mut held = self.held.clone();
        for claim in &mut held.items {
            claim.focused = claim.claim_id == self.b.claim_id;
        }
        // This changes canonical focus only. The independent producer model
        // continues accepting A's historical report until it is settled.
        self.turn.transport.replace_held_claims([Ok(held)]);
        self.turn
            .state
            .name_engram_source_root(
                &self.turn.session_id,
                EngramSourceRootRequest {
                    work: "w-next".to_owned(),
                    path: Some(Some(self.b_path.to_string_lossy().into_owned())),
                },
            )
            .unwrap()
    }
}

struct ReleaseClosingRead(Arc<(Mutex<bool>, Condvar)>);

impl Drop for ReleaseClosingRead {
    fn drop(&mut self) {
        phase_sync::set_gate(&self.0, true);
    }
}

fn send(text: &str) -> SendMessageRequest {
    SendMessageRequest {
        text: text.to_owned(),
        expanded_text: None,
        attachments: Vec::new(),
        source_session_id: None,
        source_mailbox: None,
    }
}

#[test]
fn canonical_focus_b_name_keeps_active_a_and_holds_its_queued_successor() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let f = CanonicalFocusFixture::new("canonical-focus-queued");
    f.turn
        .begin("A owns the opening and the command already in flight.");
    let original = f
        .turn
        .record(|record| record.engram.unadmitted_active_turn.clone().unwrap());
    assert_eq!(original.opening_root.as_ref().unwrap().binding, f.a);
    let original_cache = f.turn.record(|record| record.engram.named_root.clone());
    f.turn.state.note_engram_command_started(
        &f.turn.session_id,
        &EngramObservationProvenance::Ambient,
        "original-check",
        Some("cargo test --lib canonical_focus_original"),
        Some(f.a_name.root.as_ref().unwrap()),
    );
    f.turn.wait_for_snapshots();
    let b_name = f.name_focused_b();
    f.turn.state.note_engram_command_finished(
        &f.turn.session_id,
        &EngramObservationProvenance::Ambient,
        "original-check",
        "cargo test --lib canonical_focus_original",
        "running 3 tests\ntest result: ok. 3 passed",
        Some(EngramCommandExit::Code(0)),
    );
    f.turn.wait_for_snapshots();
    let after_name = f.turn.record(|record| {
        (
            record.engram.work_binding.clone(),
            record.engram.named_root.clone(),
            record.engram.unadmitted_active_turn.clone().unwrap(),
        )
    });
    assert_eq!(
        after_name.0.as_ref(),
        Some(&f.a),
        "naming B rewrote active A's cached binding"
    );
    assert_eq!(
        after_name.1, original_cache,
        "naming B rewrote active A's cached root"
    );
    assert_eq!(after_name.2.host_turn_ref, original.host_turn_ref);
    assert_eq!(after_name.2.opening_root, original.opening_root);
    let original_check = after_name
        .2
        .commands
        .iter()
        .find(|command| command.tool_key == "original-check")
        .expect("the original turn retained the command that actually completed");
    assert!(original_check.check.finished_at.is_some());
    let original_check_id = original_check.check.host_check_id.clone();
    let endpoints = f.turn.record(|record| {
        let check = record.engram.active_turn_checks.first().unwrap();
        vec![
            check.start_basis.clone(),
            check
                .end
                .as_ref()
                .expect("A's check end must be recorded when B is named mid-turn")
                .end_basis
                .clone(),
        ]
    });
    // Queue the real successor while A is active. Freeze its closing worker
    // before canonical focus/proof can publish a prospective cache refresh.
    assert!(matches!(
        f.turn
            .state
            .dispatch_turn(&f.turn.session_id, send("Controlled successor B."))
            .unwrap(),
        DispatchTurnResult::Queued
    ));
    let release = ReleaseClosingRead(Arc::new((Mutex::new(false), Condvar::new())));
    let blocked = release.0.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    *f.transport.closing_read.lock().unwrap() = Some(Box::new(move || {
        entered_tx.send(()).unwrap();
        phase_sync::wait_for_gate(&blocked, true);
    }));
    let reads_before_end = f.transport.handoff_reads();
    f.turn.end();
    receive(&entered_rx, "closing A reaches the canonical focus read");
    let reads_after_end = f.transport.handoff_reads();
    let held = f.turn.record(|record| {
        record.active_turn_generation == original.generation
            && record.queued_prompts.len() == 1
            && record.engram.unadmitted_active_turn.is_none()
            && record.engram.unadmitted_closing_jobs.iter().any(|job| {
                job.trace.host_turn_ref == original.host_turn_ref
                    && job.trace.observed_outcome == Some(EngramObservedTurnOutcome::Succeeded)
            })
    });
    let no_prompt = matches!(
        f.turn._runtime_rx.lock().unwrap().try_recv(),
        Err(mpsc::TryRecvError::Empty)
    );
    // Release on every assertion path so a failing RED cannot strand a
    // background fixture worker or turn a product failure into a hang.
    drop(release);
    for endpoint in endpoints {
        let basis = endpoint
            .wait_until(std::time::Instant::now() + DEADLOCK_GUARD)
            .unwrap()
            .unwrap();
        assert_eq!(basis.workspace_id, *f.a_name.root.as_ref().unwrap());
    }
    assert_eq!(
        reads_after_end, reads_before_end,
        "controlled handoff made an RPC"
    );
    assert!(
        held && no_prompt,
        "the controlled successor overtook A's unsettled closing capture"
    );

    let guard = phase_sync::PollGuard::new();
    loop {
        let settled =
            f.turn.record(|record| {
                record.engram.unadmitted_reports.iter().any(|report| {
                    report.host_turn_ref == original.host_turn_ref && report.is_settled()
                })
            });
        if settled {
            break;
        }
        guard.wait("A's original report settles before B's producer association changes");
    }
    f.turn.record(|record| {
        let report = record
            .engram
            .unadmitted_reports
            .iter()
            .find(|report| report.host_turn_ref == original.host_turn_ref)
            .unwrap();
        assert_eq!(report.generation, original.generation);
        for envelope in &report.envelopes {
            assert_eq!(envelope.request["binding"], json!(f.a));
            assert!(matches!(
                envelope.delivery,
                EngramRuntimeObservationDelivery::Recorded { .. }
            ));
        }
        let request = report
            .envelopes
            .iter()
            .map(|envelope| &envelope.request)
            .find(|request| request["occurrence"]["kind"] == "unadmitted_turn")
            .unwrap();
        let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
        assert_eq!(
            request["occurrence"]["host_turn_ref"],
            json!(original.host_turn_ref)
        );
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0]["host_check_id"], json!(original_check_id));
        assert_eq!(checks[0]["observed_result"], "passed");
        assert!(request.get("verification_evidence").is_none());
    });
    // The fixture's historical acceptance scope is intentionally separate
    // from canonical focus. Move it only after A's immutable report settles.
    f.turn
        .transport
        .enable_named_roots(&f.turn.session_id, &f.b);
    let prompt = delivered_naming_prompt(&f.turn);
    assert!(prompt.contains("Controlled successor B."));
    f.turn.record(|record| {
        let next = record.engram.unadmitted_active_turn.as_ref().unwrap();
        assert_ne!(next.host_turn_ref, original.host_turn_ref);
        assert!(next.generation > original.generation);
        let opening = next.opening_root.as_ref().unwrap();
        assert_eq!(opening.binding, f.b);
        let root = record.engram.observed_source_root().unwrap();
        assert_eq!(root.generation, b_name.generation);
        assert_eq!(root.root, *b_name.root.as_ref().unwrap());
    });
    assert_ordinary_turn_has_no_native_requests(&f.turn);
    f.turn.end();
}
