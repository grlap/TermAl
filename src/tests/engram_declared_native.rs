// Disposable host/provider telemetry fixtures. No consumer suite is executed,
// and these synthetic artifacts are not consumer verification evidence.
use super::*;
use crate::tests::engram_declared_source_fixture::NativeSourceOwner;

#[path = "engram_declared_task_artifacts.rs"]
mod task_artifacts;

#[path = "acceptance_native_registration.rs"]
mod acceptance_admission;

struct NativeFixture {
    source_owner: NativeSourceOwner,
    fixture: RunFixture,
    session: String,
    token: RuntimeToken,
    provenance: EngramObservationProvenance,
    claude: bool,
    claude_state: Mutex<ClaudeTurnState>,
}

#[test]
fn native_opening_keeps_its_local_binding_without_reading_held_claims() {
    for claude in [false, true] {
        let native = NativeFixture::new("native-local-opening", claude);
        let expected = {
            let inner = native.fixture.state.inner.lock().unwrap();
            test_run_registration_named_binding(
                &inner,
                &native.session,
                inner
                    .projects
                    .iter()
                    .find(|project| project.id == native.fixture.project_id)
                    .unwrap()
                    .engram
                    .as_ref()
                    .unwrap()
                    .authority_store_key
                    .as_ref(),
                test_run_worktree_keys(&native.fixture.root).as_ref(),
            )
            .unwrap()
        };
        let transport = ScriptedEngramControlTransport::new([]);
        transport.replace_held_claims([Err(EngramTransportError::deadline(
            "authority is unavailable",
        ))]);
        native
            .fixture
            .state
            .install_test_engram_transport(transport.clone());
        native.prepare();
        assert_eq!(transport.held_claim_reads(), 0, "S0 uses no authority RPC");
        let inner = native.fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&native.session).unwrap()];
        let turn = record.engram.declared_run_turn.as_ref().unwrap();
        assert_eq!(
            (Some(turn.binding.clone()), Some(turn.named.clone())),
            expected
        );
        assert!(turn.snapshot.is_some());
        assert!(declared_run_opening_notice(record).is_none());
    }
}

#[test]
fn native_opening_uses_only_the_acknowledged_binding_for_its_exact_selection() {
    for claude in [false, true] {
        for case in [
            "current",
            "other-work",
            "other-claim",
            "other-run",
            "other-root-execution",
            "old-fence",
            "new-fence",
            "old-revision",
            "unpublished",
            "missing-owner",
            "missing-binding",
            "foreign-selection",
            "unresolved",
            "changed-during-capture",
        ] {
            let native = NativeFixture::new("native-local-recovered-binding", claude);
            let mut expected = {
                let mut inner = native.fixture.state.inner.lock().unwrap();
                let root = &mut inner.engram_work_source_roots[0];
                root.claim_fence += 1;
                let fence = root.claim_fence;
                let selection = root.clone();
                let history = &mut inner.engram_work_naming_history[0];
                let mut binding = history.proofs[0].binding.clone();
                binding.claim_fence = fence;
                binding.work_revision += 1;
                history.epoch = ENGRAM_NAMING_HISTORY_EPOCH;
                history.owner_version = 1;
                history.transition = Some(EngramAuthorityTransition {
                    id: "published-recovered-name".into(),
                    version: 1,
                    binding: history.proofs[0].binding.clone(),
                    phase: EngramAuthorityPhase::Published,
                });
                history.acknowledged_root_binding = Some(EngramAcknowledgedRootBinding {
                    selection,
                    binding: binding.clone(),
                });
                binding
            };
            {
                let mut inner = native.fixture.state.inner.lock().unwrap();
                let history = &mut inner.engram_work_naming_history[0];
                let acknowledged = history.acknowledged_root_binding.as_mut().unwrap();
                match case {
                    "other-work" => acknowledged.binding.work_id.push_str("-other"),
                    "other-claim" => acknowledged.binding.claim_id.push_str("-other"),
                    "other-run" => acknowledged.binding.run_id.push_str("-other"),
                    "other-root-execution" => {
                        acknowledged.binding.root_execution_id.push_str("-other")
                    }
                    "old-fence" => acknowledged.binding.claim_fence -= 1,
                    "new-fence" => acknowledged.binding.claim_fence += 1,
                    "old-revision" => acknowledged.binding.work_revision -= 2,
                    "foreign-selection" => {
                        acknowledged.selection.named_by_session.push_str("-other")
                    }
                    "unpublished" => {
                        history.transition.as_mut().unwrap().phase = EngramAuthorityPhase::Prepared
                    }
                    "unresolved" => history.unresolved_runs.push(expected.clone()),
                    "missing-owner" => history.transition = None,
                    "missing-binding" => history.acknowledged_root_binding = None,
                    _ => {}
                }
            }
            let snapshot = native
                .source_owner
                .snapshot(std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT, None)
                .unwrap();
            let state = Arc::downgrade(&native.fixture.state.inner);
            let move_owner = case == "changed-during-capture";
            let captures = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let seen = captures.clone();
            native
                .fixture
                .state
                .inner
                .lock()
                .unwrap()
                .test_declared_run_snapshot = Some(Arc::new(move |_, _| {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if move_owner {
                    state
                        .upgrade()
                        .unwrap()
                        .lock()
                        .unwrap()
                        .engram_work_naming_history[0]
                        .acknowledged_root_binding
                        .as_mut()
                        .unwrap()
                        .binding
                        .work_revision += 1;
                }
                Ok(snapshot.clone())
            }));
            let transport = ScriptedEngramControlTransport::new([]);
            transport.replace_held_claims([Err(EngramTransportError::deadline(
                "no opening authority RPC",
            ))]);
            native
                .fixture
                .state
                .install_test_engram_transport(transport.clone());
            native.fixture.state.prepare_declared_run_before_handoff(
                &native.session,
                &native.token,
                1,
            );
            assert_eq!(transport.held_claim_reads(), 0, "{case}");
            let inner = native.fixture.state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&native.session).unwrap()];
            let opening = record.engram.declared_run_turn.as_ref();
            if case == "current" {
                let opening = opening.unwrap();
                assert_eq!(opening.binding, expected, "{case}");
                assert!(opening.snapshot.is_some());
                assert_eq!(captures.load(std::sync::atomic::Ordering::SeqCst), 1);
                expected.claim_fence -= 1;
                expected.work_revision -= 1;
                assert_eq!(
                    inner.engram_work_naming_history[0].proofs[0].binding, expected,
                    "the retained recovery proof stays historical"
                );
            } else {
                assert!(
                    opening.is_none_or(|opening| opening.snapshot.is_none()),
                    "{case}"
                );
                assert_eq!(
                    captures.load(std::sync::atomic::Ordering::SeqCst),
                    usize::from(move_owner),
                    "{case}"
                );
            }
        }
    }
}

#[test]
fn native_opening_old_format_history_falls_back_only_at_the_proven_fence() {
    for claude in [false, true] {
        let native = NativeFixture::new("native-old-format-history", claude);
        {
            let mut inner = native.fixture.state.inner.lock().unwrap();
            let mut old = serde_json::to_value(&inner.engram_work_naming_history[0]).unwrap();
            old.as_object_mut()
                .unwrap()
                .remove("acknowledgedRootBinding");
            inner.engram_work_naming_history[0] = serde_json::from_value(old).unwrap();
            assert!(
                inner.engram_work_naming_history[0]
                    .acknowledged_root_binding
                    .is_none()
            );
        }
        native.prepare();
        let original = {
            let mut inner = native.fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&native.session).unwrap();
            let original = inner.sessions[index]
                .engram
                .declared_run_turn
                .as_ref()
                .unwrap()
                .binding
                .clone();
            inner.engram_work_source_roots[0].claim_fence += 1;
            original
        };
        native
            .fixture
            .state
            .prepare_declared_run_before_handoff(&native.session, &native.token, 1);
        let inner = native.fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&native.session).unwrap()];
        assert!(
            record.engram.declared_run_turn.is_none(),
            "missing refreshed authority cannot borrow an old proof"
        );
        assert_eq!(
            inner.engram_work_naming_history[0].proofs[0].binding,
            original
        );
    }
}

#[test]
fn native_registration_keeps_forward_revision_and_closing_rejects_later_movement() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for claude in [false, true] {
        for closing_revision in [4, 5, 3] {
            let fixture = NativeFixture::new("native-registration-revision", claude);
            fixture.held_revision(4);
            fixture.prepare();
            let mut recorder = fixture.recorder();
            fixture.start(&mut recorder);
            let original = fixture.wait();
            assert_eq!(
                original.binding.work_revision, 3,
                "naming proof stays immutable"
            );
            let registered_binding = {
                let inner = fixture.fixture.state.inner.lock().unwrap();
                assert_eq!(
                    inner.engram_work_naming_history[0].proofs[0]
                        .binding
                        .work_revision,
                    3
                );
                let registration = &inner.test_run_registrations[&original.run_id];
                assert_eq!(registration.declared_native.as_ref(), Some(&original));
                let binding = registration.work_binding.clone().unwrap();
                assert_eq!(
                    binding.work_revision, 4,
                    "registration retains the forward revision"
                );
                binding
            };
            let persisted = load_state(fixture.fixture.state.persistence_path.as_path())
                .unwrap()
                .unwrap();
            assert_eq!(
                persisted.test_run_registrations[&original.run_id].work_binding,
                Some(registered_binding.clone()),
                "the registration baseline is durable"
            );
            // Only change the authority after registration has settled. This
            // must not be accepted as another forward refresh at closing.
            fixture.held_revision(closing_revision);
            fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
            let (start, registration, request, results) = fixture.finish(&mut recorder, 0);
            assert_eq!(start, original);
            assert_eq!(registration.work_binding, Some(registered_binding));
            assert_eq!(registration.declared_native.as_ref(), Some(&original));
            assert_eq!(
                results["state"], "passed",
                "the actual artifact remains visible"
            );
            assert_eq!(results["nativeEligible"], closing_revision == 4);
            let body = engram_verification_record_body(&registration, &request, &results);
            if closing_revision == 4 {
                assert_eq!(
                    body.unwrap().run.outcome,
                    EngramVerificationRecordRunOutcome::Passed
                );
            } else {
                assert!(
                    body.unwrap_err()
                        .contains("ineligible under its host authority checks")
                );
            }
        }
    }
}

struct ParkedNativeClaims {
    gate: Arc<(Mutex<bool>, Condvar)>,
    entered: Mutex<Option<mpsc::Sender<()>>>,
    calls: std::sync::atomic::AtomicUsize,
}

impl EngramControlTransport for ParkedNativeClaims {
    fn request(
        &self,
        _: &EngramConnectionConfig,
        _: &EngramControlRequest,
        _: Duration,
    ) -> Result<Value, EngramTransportError> {
        Err(EngramTransportError::protocol(
            "no control request expected",
        ))
    }

    fn read_held_claims(
        &self,
        _: &EngramConnectionConfig,
        _: Duration,
    ) -> Result<EngramHeldClaims, EngramTransportError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(entered) = self.entered.lock().unwrap().take() {
            entered.send(()).unwrap();
        }
        phase_sync::wait_for_gate(&self.gate, true);
        Err(EngramTransportError::deadline(
            "parked held claims released",
        ))
    }

    fn shutdown_session(&self, _: &str) {}
}

#[test]
fn native_handoff_and_shared_reader_progress_before_a_held_claim_answer() {
    let native = NativeFixture::new("native-reader-opening", false);
    native.prepare();
    {
        let mut inner = native.fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&native.session).unwrap();
        inner.sessions[index].engram.declared_run_turn = None;
    }
    let process = phase_sync::ParkedProcess::spawn();
    let (runtime, _, _) =
        test_shared_codex_runtime_with_process("native-reader", process.process.clone());
    let (commands_tx, commands_rx) = mpsc::channel();
    let dispatch = TurnDispatch::PersistentCodex {
        human_prompt_id: None,
        federation_handoff: None,
        active_turn_generation: 1,
        opening_prompt: None,
        mailbox_notification: None,
        runtime_token: native.token.clone(),
        sender: commands_tx,
        session_id: native.session.clone(),
        service_tier: Ok(None),
        command: CodexPromptCommand {
            active_turn_generation: 1,
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: test_run_display_path(&native.fixture.root),
            model: "gpt-5.4".into(),
            prompt: "Native declared opening.".into(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
            delivery_receipt: None,
        },
    };
    let pending: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let (reply_tx, reply_rx) = mpsc::channel();
    pending
        .lock()
        .unwrap()
        .insert("unrelated-native-reply".into(), reply_tx);
    std::thread::scope(|scope| {
        // Drop before scoped joins, including on a blocked-reader assertion.
        let release = phase_sync::ReleaseGateOnDrop::new();
        let (entered_tx, entered_rx) = mpsc::channel();
        let authority = Arc::new(ParkedNativeClaims {
            gate: release.gate(),
            entered: Mutex::new(Some(entered_tx)),
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        native
            .fixture
            .state
            .install_test_engram_transport(authority.clone());
        let target = AppState::engram_source_root_target_locked(
            &native.fixture.state.inner.lock().unwrap(),
            &native.session,
        )
        .unwrap()
        .unwrap();
        let authority_reader = authority.clone();
        let parked = scope.spawn(move || {
            authority_reader
                .read_held_claims(&target.connection, ENGRAM_WORK_BINDING_COMMAND_TIMEOUT)
        });
        phase_sync::receive(&entered_rx, "held answer parked before native handoff");
        let state = &native.fixture.state;
        let reader = scope.spawn(move || {
            assert!(matches!(
                deliver_turn_dispatch(state, dispatch),
                TurnDispatchDeliveryOutcome::Delivered
            ));
            handle_shared_codex_app_server_message(
                &json!({"id":"unrelated-native-reply", "result":{"routed":true}}),
                state,
                &runtime.runtime_id,
                &pending,
                &runtime.sessions,
                &runtime.thread_sessions,
                &runtime.input_tx,
            )
            .unwrap();
        });
        assert!(
            matches!(phase_sync::receive(&commands_rx, "native provider enqueue before authority answer"),
            CodexRuntimeCommand::Prompt { session_id, .. } if session_id == native.session)
        );
        assert_eq!(
            phase_sync::receive(&reply_rx, "shared reader routes a second response").unwrap(),
            json!({"routed":true})
        );
        assert_eq!(
            authority.calls.load(Ordering::SeqCst),
            1,
            "only the explicit parked read called Engram"
        );
        assert!(
            !*release.gate().0.lock().unwrap(),
            "delivery preceded the parked answer"
        );
        let inner = native.fixture.state.inner.lock().unwrap();
        let turn = inner.sessions[inner.find_session_index(&native.session).unwrap()]
            .engram
            .declared_run_turn
            .as_ref()
            .unwrap();
        assert!(
            turn.snapshot.is_some(),
            "the actual handoff still prepares S0 for its turn"
        );
        drop(inner);
        reader.join().unwrap();
        release.release();
        assert!(parked.join().unwrap().is_err());
    });
}

#[test]
fn native_registration_refuses_later_authority_without_relabelling_the_original_start() {
    for claude in [false, true] {
        for changed in [false, true] {
            let native = NativeFixture::new("native-later-authority", claude);
            native.prepare();
            let transport = ScriptedEngramControlTransport::new([]);
            if changed {
                let mut binding = native
                    .fixture
                    .state
                    .inner
                    .lock()
                    .unwrap()
                    .engram_work_naming_history[0]
                    .proofs[0]
                    .binding
                    .clone();
                binding.run_id = "replacement-recovery-run".into();
                transport.replace_held_claims([Ok(EngramHeldClaims {
                    items: vec![EngramHeldClaim {
                        work_id: binding.work_id.clone(),
                        short_ref: "changed-native".into(),
                        claim_id: binding.claim_id.clone(),
                        claim_fence: binding.claim_fence,
                        focused: false,
                        control_binding: Some(binding),
                    }],
                    omitted: 0,
                })]);
            } else {
                transport.replace_held_claims([Err(EngramTransportError::deadline(
                    "later authority unavailable",
                ))]);
            }
            let release = phase_sync::ReleaseGateOnDrop::new();
            let gate = release.gate();
            let (entered_tx, entered_rx) = mpsc::channel();
            transport.on_next_held_claim_read(move |_| {
                entered_tx.send(()).unwrap();
                phase_sync::wait_for_gate(&gate, true);
            });
            native
                .fixture
                .state
                .install_test_engram_transport(transport.clone());
            let mut recorder = native.recorder();
            native.start(&mut recorder);
            phase_sync::receive(
                &entered_rx,
                "registration checks authority after the original start",
            );
            let execution = native.execution().unwrap();
            let original = execution.lock().unwrap().reservation.clone();
            assert_eq!(
                original.binding.work_revision, 3,
                "the locally captured binding is immutable"
            );
            native.artifact(include_str!("fixtures/declared-trx/pass.trx"));
            native.end(&mut recorder, 0);
            release.release();
            let guard = phase_sync::PollGuard::new();
            loop {
                if !execution.lock().unwrap().worker_active {
                    break;
                }
                guard.wait("native registration authority refusal");
            }
            let job = execution.lock().unwrap();
            assert_eq!(job.reservation, original);
            assert!(!job.registered);
            let reason = job.failure.as_deref().unwrap();
            assert!(
                reason.contains(if changed {
                    "held association changed"
                } else {
                    "held-claim read"
                }),
                "{reason}"
            );
            drop(job);
            assert_eq!(
                transport.held_claim_reads(),
                1,
                "a failed attribution does not retry or recapture S0"
            );
            let inner = native.fixture.state.inner.lock().unwrap();
            assert!(!inner.test_run_registrations.contains_key(&original.run_id));
            let row = &inner.test_run_observations[&original.execution_id];
            assert_eq!(row.declared_native.as_ref(), Some(&original));
            assert_eq!(
                row.summary.state,
                TestRunState::Passed,
                "the real artifact result stays visible without credit"
            );
            assert_eq!(row.summary.exit_code, if claude { None } else { Some(0) });
            assert!(row.summary.ended_at.is_some());
        }
    }
}

#[test]
fn native_opening_stage_diagnostics_are_bounded_and_keep_the_active_operation() {
    let timings = Arc::new(SourceReadTimings::default());
    for _ in 0..40 {
        drop(timings.begin("git_config"));
    }
    let active = timings.begin("git_cat_file");
    let summary = timings.summary();
    assert!(
        summary.contains("earlier_stages_omitted") && summary.contains("git_cat_file:running:")
    );
    assert!(summary.len() <= 2048);
    drop(active);
    assert!(!timings.summary().contains(":running:"));
}

#[test]
fn native_opening_without_declaration_skips_the_capture_worker() {
    let native = NativeFixture::new("native-no-declaration", false);
    fs::remove_file(native.fixture.root.join(ENGRAM_DECLARATION_FILE)).unwrap();
    let captures = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = captures.clone();
    *native
        .fixture
        .state
        .capture_lifetime
        .before_capture
        .lock()
        .unwrap() = Some(Arc::new(move || {
        seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));
    native
        .fixture
        .state
        .prepare_declared_run_before_handoff(&native.session, &native.token, 1);
    let inner = native.fixture.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&native.session).unwrap()];
    assert!(record.engram.declared_run_turn.is_none());
    assert!(
        declared_run_opening_notice(record).is_none(),
        "non-applicable sessions get no native warning"
    );
    assert_eq!(captures.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn native_opening_ignored_uncommitted_declaration_has_a_named_agent_notice() {
    let native = NativeFixture::new("native-uncommitted-declaration", false);
    fs::write(
        native.fixture.root.join(".gitignore"),
        "/.tmp/\n/termal-tests.toml\n",
    )
    .unwrap();
    run_git_test_command(
        &native.fixture.root,
        &["rm", "--cached", "--", ENGRAM_DECLARATION_FILE],
    );
    run_git_test_command(&native.fixture.root, &["add", "--", ".gitignore"]);
    run_git_test_command(
        &native.fixture.root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--quiet",
            "--no-gpg-sign",
            "-m",
            "ignored native declaration control",
        ],
    );
    native.source_owner.refresh_commit();
    let source = native
        .source_owner
        .snapshot(std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT, None)
        .map_err(|error| format!("{error:#}"));
    assert!(
        source.is_err(),
        "native declaration membership refuses this uncommitted path"
    );
    native.install_snapshot(source);
    native
        .fixture
        .state
        .prepare_declared_run_before_handoff(&native.session, &native.token, 1);
    let inner = native.fixture.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&native.session).unwrap()];
    let turn = record.engram.declared_run_turn.as_ref().unwrap();
    assert!(turn.snapshot.is_none());
    assert_eq!(turn.opening_refusal_code, Some("declaration_not_committed"));
    let notice = declared_run_opening_notice(record).unwrap();
    assert!(
        notice.contains("(declaration_not_committed)") && notice.contains("The prompt proceeds."),
        "{notice}"
    );
}

#[test]
fn native_opening_expiry_notice_is_turn_scoped_for_both_providers() {
    for claude in [false, true] {
        let native = NativeFixture::new("native-opening-notice", claude);
        native.prepare();
        let expected = {
            let mut inner = native.fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&native.session).unwrap();
            let turn = inner.sessions[index]
                .engram
                .declared_run_turn
                .as_mut()
                .unwrap();
            turn.snapshot = None;
            turn.clone()
        };
        native.fixture.state.declared_run_opening_refused(&native.session, &expected,
            "declaration_not_committed: membership read failed: bounded read deadline exceeded (TimedOut)");
        let mut inner = native.fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&native.session).unwrap();
        let record = &mut inner.sessions[index];
        assert!(
            record
                .engram
                .declared_run_turn
                .as_ref()
                .unwrap()
                .snapshot
                .is_none()
        );
        let notice = declared_run_opening_notice(record).unwrap();
        assert!(
            notice.contains("(opening_deadline)")
                && notice.contains("first tool activity of a later turn"),
            "{notice}"
        );
        record.active_turn_generation += 1;
        assert!(
            declared_run_opening_notice(record).is_none(),
            "an old refusal cannot attach to another turn"
        );
    }
}

#[test]
fn native_opening_publication_and_expiry_use_a_controlled_clock() {
    let total = NATIVE_S0_CAPTURE_BUDGET + NATIVE_S0_AUTHORITY_MARGIN;
    for elapsed in [
        Duration::from_secs(9),
        total - Duration::from_millis(1),
        total,
        total + Duration::from_millis(1),
    ] {
        let expired = elapsed >= total;
        let native = NativeFixture::new("native-controlled-opening", false);
        // Exercise native snapshot assembly with the scripted shared owner,
        // separately from dispatch availability or real-producer latency.
        let snapshot = native
            .source_owner
            .snapshot(std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT, None)
            .unwrap();
        let initial = std::time::Instant::now();
        let clock = Arc::new(Mutex::new(initial));
        let read_clock = clock.clone();
        native
            .fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_declared_run_clock = Some(Arc::new(move || *read_clock.lock().unwrap()));
        native
            .fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_declared_run_snapshot = Some(Arc::new(move |_, _| {
            // Controlled ordering: capture completes before publication, with
            // logical time either still valid or beyond the original deadline.
            // Exercise both sides and the exact original admission boundary.
            *clock.lock().unwrap() = initial + elapsed;
            Ok(snapshot.clone())
        }));
        native
            .fixture
            .state
            .prepare_declared_run_before_handoff(&native.session, &native.token, 1);
        let inner = native.fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&native.session).unwrap()];
        let turn = record.engram.declared_run_turn.as_ref().unwrap();
        assert_eq!(turn.snapshot.is_some(), !expired);
        if expired {
            assert_eq!(turn.opening_refusal_code, Some("opening_deadline"));
            assert!(
                declared_run_opening_notice(record)
                    .unwrap()
                    .contains("S0 stages:")
            );
        }
    }
}

#[test]
fn native_opening_observes_the_named_worktree_once_without_borrowing_project_root_metadata() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut native = NativeFixture::new("native-opening-distinct-root", false);
    let named_parent = native.fixture.root.join(".tmp");
    // Windows path identity checks canonicalize their input. Convert the
    // existing parent; Git creates the new child worktree below it.
    let named_path = format!(
        "{}/named-root",
        identity_git_path(&named_parent).unwrap().replace('\\', "/")
    );
    run_git_test_command(
        &native.fixture.root,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            &named_path,
            "HEAD",
        ],
    );
    let named_root = fs::canonicalize(named_parent.join("named-root")).unwrap();
    let (store, binding) = {
        let mut inner = native.fixture.state.inner.lock().unwrap();
        let store = inner.engram_work_source_roots[0].store.clone();
        let binding = inner.engram_work_naming_history[0].proofs[0]
            .binding
            .clone();
        inner.engram_work_source_roots.clear();
        inner.engram_work_naming_history.clear();
        (store, binding)
    };
    named_root::name_root(
        &native.fixture,
        &store,
        &binding,
        &native.session,
        &named_root,
        2,
    );
    native.source_owner = NativeSourceOwner::new(&named_root);
    native
        .source_owner
        .install(&native.fixture.state, &native.fixture.project_id);
    native.prepare();
    let pending = {
        let mut inner = native.fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&native.session).unwrap();
        let pending = inner.sessions[index]
            .engram
            .declared_run_turn
            .as_mut()
            .unwrap();
        pending.snapshot = None;
        pending.clone()
    };
    let expected = native.source_owner.identity();
    let expected_cwd = engram_host_workdir(native.fixture.state.persistence_path.as_ref()).unwrap();
    let mut roots = Vec::new();
    let deadline =
        std::time::Instant::now() + NATIVE_S0_CAPTURE_BUDGET + NATIVE_S0_AUTHORITY_MARGIN;
    let snapshot = native
        .fixture
        .state
        .capture_declared_run_opening_with_owner(
            &native.session,
            &pending,
            deadline,
            EngramSourcePhasePlan::from(std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET),
            |_, cwd, args, until, _| {
                assert_eq!(
                    cwd, expected_cwd,
                    "the measured root is an argument, not the host process cwd"
                );
                assert!(
                    native
                        .fixture
                        .state
                        .inner
                        .is_not_held_by_current_thread_for_test(),
                    "source IO holds no StateInner lock"
                );
                assert!(
                    until < deadline,
                    "source must leave the outer authority reserve available"
                );
                if args[0] == "--version" {
                    return Ok(super::engram_source_owner::source_owner_reply(args));
                }
                assert_eq!(args[0], "source");
                assert_eq!(args.len(), 4, "use the owner's supported default");
                let root = fs::canonicalize(&args[2]).unwrap();
                roots.push(root);
                Ok(super::engram_source_owner::source_owner_output(
                    0,
                    serde_json::to_vec(&json!({
                        "version":1,"status":"clean","root":args[2],
                        "source":format!("git-commit:{}", expected.commit),"tree":expected.tree,
                        "entries":[],"refusal":null,"source_id":expected,
                    }))
                    .unwrap(),
                ))
            },
        );
    assert_eq!(
        roots,
        vec![named_root.clone()],
        "S0 must use one actual named-root observation, with no project-root measurement or second read"
    );
    let (snapshot, _) = snapshot.unwrap();
    assert_eq!(snapshot.source.root, named_root);
    assert_eq!(snapshot.source.source_id, expected);
    assert_eq!(snapshot.source.source_owner, native.source_owner.pin);
}

#[test]
fn native_opening_rechecks_pending_root_fence_and_owner_authority_before_returning_s0() {
    for change in [
        "root",
        "fence",
        "configuration",
        "turn",
        "native-identity",
        "first-tool",
        "caller-root",
    ] {
        let native = NativeFixture::new("native-opening-authority-recheck", false);
        native.prepare();
        let mut pending = {
            let mut inner = native.fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&native.session).unwrap();
            let pending = inner.sessions[index]
                .engram
                .declared_run_turn
                .as_mut()
                .unwrap();
            pending.snapshot = None;
            pending.clone()
        };
        if change == "caller-root" {
            pending.named.workspace_id = test_run_display_path(&native.fixture.root.join(".tmp"));
        }
        let expected = native.source_owner.identity();
        let mut captures = 0;
        let result = native
            .fixture
            .state
            .capture_declared_run_opening_with_owner(
                &native.session,
                &pending,
                std::time::Instant::now() + NATIVE_S0_CAPTURE_BUDGET,
                EngramSourcePhasePlan::from(
                    std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET,
                ),
                |_, _, args, _, _| {
                    if args[0] == "--version" {
                        return Ok(super::engram_source_owner::source_owner_reply(args));
                    }
                    captures += 1;
                    let mut inner = native
                        .fixture
                        .state
                        .inner
                        .try_lock_now()
                        .expect("off-lock owner capture");
                    let index = inner.find_session_index(&native.session).unwrap();
                    match change {
                        "root" => inner.engram_work_source_roots[0].generation += 1,
                        "fence" => inner.engram_work_source_roots[0].claim_fence += 1,
                        "configuration" => {
                            inner
                                .projects
                                .iter_mut()
                                .find(|project| project.id == native.fixture.project_id)
                                .unwrap()
                                .engram
                                .as_mut()
                                .unwrap()
                                .binary_path = Some("different-owner".into())
                        }
                        "turn" => inner.sessions[index].active_turn_generation += 1,
                        "native-identity" => {
                            inner.sessions[index].federation_native_creation_id =
                                Uuid::new_v4().to_string()
                        }
                        "first-tool" => inner.sessions[index]
                            .engram
                            .declared_run_turn
                            .as_mut()
                            .unwrap()
                            .first_tool
                            .observe(Some("earlier-tool"), false),
                        _ => panic!(
                            "an arbitrary caller root must be refused before any source process"
                        ),
                    }
                    drop(inner);
                    Ok(super::engram_source_owner::source_owner_output(
                        0,
                        serde_json::to_vec(&json!({
                            "version":1,"status":"clean","root":args[2],
                            "source":format!("git-commit:{}", expected.commit),"tree":expected.tree,
                            "entries":[],"refusal":null,"source_id":expected,
                        }))
                        .unwrap(),
                    ))
                },
            );
        assert!(
            result.is_err(),
            "{change}: changed authority cannot publish S0"
        );
        assert_eq!(
            captures,
            usize::from(change != "caller-root"),
            "{change}: no extra source read"
        );
        let inner = native.fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&native.session).unwrap()];
        assert!(
            record
                .engram
                .declared_run_turn
                .as_ref()
                .unwrap()
                .snapshot
                .is_none(),
            "{change}"
        );
    }
}

#[test]
fn native_opening_owner_authority_is_rechecked_at_final_publication() {
    let native = NativeFixture::new("native-opening-publication-authority", false);
    let snapshot = native
        .source_owner
        .snapshot(std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT, None)
        .unwrap();
    let state = native.fixture.state.clone();
    let session = native.session.clone();
    native
        .fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_declared_run_snapshot = Some(Arc::new(move |_, _| {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        // Existing runtime/root/claim checks still match. The owner admission's
        // captured native identity must also survive the publication boundary.
        inner.sessions[index].federation_native_creation_id = Uuid::new_v4().to_string();
        Ok(snapshot.clone())
    }));
    native
        .fixture
        .state
        .prepare_declared_run_before_handoff(&native.session, &native.token, 1);
    let inner = native.fixture.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&native.session).unwrap()];
    assert!(
        record
            .engram
            .declared_run_turn
            .as_ref()
            .unwrap()
            .snapshot
            .is_none()
    );
}

impl NativeFixture {
    fn new(label: &str, claude: bool) -> Self {
        let fixture = RunFixture::new(label);
        fs::write(fixture.root.join(".gitignore"), "/.tmp/\n").unwrap();
        fs::write(
            fixture.root.join("termal-tests.toml"),
            "[[test]]\ncommand = \"dotnet test\"\ncwd = \".\"\nartifact = \".tmp/test.trx\"\n",
        )
        .unwrap();
        run_git_test_command(
            &fixture.root,
            &["add", "--", ".gitignore", "termal-tests.toml"],
        );
        run_git_test_command(
            &fixture.root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--quiet",
                "--no-gpg-sign",
                "-m",
                "native telemetry fixture",
            ],
        );
        let session = running_owner(&fixture, "native-declared-runtime");
        let token = if claude {
            let (runtime, _commands) = test_claude_runtime_handle("native-declared-runtime");
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
            RuntimeToken::Claude("native-declared-runtime".into())
        } else {
            RuntimeToken::Codex("native-declared-runtime".into())
        };
        let store = EngramAuthorityStoreKey {
            database_path: fixture.root.join("tracker.db"),
            project_id: "native-test".into(),
        };
        let binding = EngramControlWorkBinding {
            root_execution_id: "root-native".into(),
            work_id: "work-native".into(),
            run_id: "run-native".into(),
            work_revision: 3,
            claim_id: "claim-native".into(),
            claim_fence: 2,
        };
        named_root::name_root(
            &fixture,
            &store,
            &binding,
            &session,
            &fs::canonicalize(&fixture.root).unwrap(),
            1,
        );
        {
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].session.status = SessionStatus::Active;
            inner.sessions[index].active_turn_generation = 1;
            // Registered committed runs must work with ordinary control OFF.
            inner.sessions[index].engram.work_binding = None;
        }
        let provenance = if claude {
            EngramObservationProvenance::Claude(ClaudeObservationProvenance {
                token: token.clone(),
                origin: ClaudeWorkOrigin::Attempt { turn_generation: 1 },
            })
        } else {
            EngramObservationProvenance::Runtime {
                token: token.clone(),
                turn_generation: 1,
            }
        };
        let source_owner = NativeSourceOwner::new(&fixture.root);
        source_owner.install(&fixture.state, &fixture.project_id);
        Self {
            fixture,
            session,
            token,
            provenance,
            claude,
            source_owner,
            claude_state: Mutex::new(ClaudeTurnState::default()),
        }
    }

    fn prepare(&self) {
        // Script the owner value for protocol tests. M1's real parser/pin
        // checks and the declaration membership run; E1 owns Git conformance.
        self.source_owner.refresh_commit();
        let snapshot = self
            .source_owner
            .snapshot(std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT, None)
            .unwrap();
        self.install_snapshot(Ok(snapshot));
        self.fixture
            .state
            .prepare_declared_run_before_handoff(&self.session, &self.token, 1);
        let inner = self.fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&self.session).unwrap()];
        let opening = record.engram.declared_run_turn.as_ref().unwrap();
        if opening.snapshot.is_none() {
            self.fixture
                .state
                .test_temp_root
                .as_ref()
                .unwrap()
                .retain_for_diagnosis();
        }
        assert!(
            opening.snapshot.is_some(),
            "S0 for {}: {:?}",
            self.fixture.root.display(),
            opening.opening_refusal
        );
    }

    fn install_snapshot(&self, snapshot: std::result::Result<DeclaredRunSnapshot, String>) {
        self.fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_declared_run_snapshot = Some(Arc::new(move |_, _| {
            snapshot.clone().map_err(|error| anyhow!(error))
        }));
    }

    fn record_change(&self, path: &str, status: &str) {
        self.source_owner.record_file_change(path, status);
    }

    fn held_revision(&self, revision: i64) {
        // The naming proof stays at revision 3. Registration may retain a
        // newer revision without rewriting the original opening reservation.
        let mut binding = {
            let inner = self.fixture.state.inner.lock().unwrap();
            inner.engram_work_naming_history[0].proofs[0]
                .binding
                .clone()
        };
        binding.work_revision = revision;
        let transport = ScriptedEngramControlTransport::new([]);
        transport.replace_held_claims([Ok(EngramHeldClaims {
            items: vec![EngramHeldClaim {
                work_id: binding.work_id.clone(),
                short_ref: "native-test".into(),
                claim_id: binding.claim_id.clone(),
                claim_fence: binding.claim_fence,
                focused: false,
                control_binding: Some(binding),
            }],
            omitted: 0,
        })]);
        self.fixture.state.install_test_engram_transport(transport);
    }

    fn recorder(&self) -> SessionRecorder {
        let mut recorder = SessionRecorder::new(self.fixture.state.clone(), self.session.clone());
        recorder.recorder_state.observation_provenance = self.provenance.clone();
        recorder.recorder_state.native_tool_provenance = Some(self.provenance.clone());
        recorder
    }

    fn frame(&self) -> Value {
        if self.claude {
            let path = identity_git_path(&fs::canonicalize(&self.fixture.root).unwrap())
                .unwrap()
                .replace('\\', "/");
            json!({"id":"native-tool","name":"Bash","timestamp":"2000-01-01T00:00:00Z",
                "input":{"command":format!("pushd \"{path}\" && dotnet test")}})
        } else {
            json!({"id":"native-tool","type":"commandExecution","command":"dotnet test",
                "cwd":test_run_display_path(&self.fixture.root),"timestamp":"2000-01-01T00:00:00Z"})
        }
    }

    fn start(&self, recorder: &mut SessionRecorder) {
        let frame = self.frame();
        if self.claude {
            register_claude_tool_use(&frame, &mut self.claude_state.lock().unwrap(), recorder)
                .unwrap();
        } else {
            handle_codex_app_server_item_started(&frame, recorder).unwrap();
        }
    }

    fn execution(&self) -> Option<Arc<Mutex<DeclaredRunExecution>>> {
        let inner = self.fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&self.session).unwrap()];
        record
            .engram
            .declared_run_turn
            .as_ref()
            .and_then(|turn| turn.execution.clone())
    }

    fn wait(&self) -> DeclaredRunReservation {
        let execution = self.execution().expect("original start reserved");
        let deadline = std::time::Instant::now() + crate::TEST_PHASE_DEADLOCK_GUARD;
        loop {
            let job = execution.lock().unwrap();
            assert!(job.failure.is_none(), "{:?}", job.failure);
            if !job.worker_active {
                return job.reservation.clone();
            }
            drop(job);
            assert!(
                std::time::Instant::now() < deadline,
                "native worker did not settle"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn end(&self, recorder: &mut SessionRecorder, exit: i64) {
        if self.claude {
            handle_claude_tool_result(&json!({"message":{"content":[{"type":"tool_result",
                "tool_use_id":"native-tool", "is_error":exit != 0,
                "content":if exit == 0 {"actual provider output".to_owned()} else {format!("Exit code {exit}")}}]}}),
                &mut self.claude_state.lock().unwrap(), recorder).unwrap();
        } else {
            recorder
                .command_completed_with_exit(
                    "native-tool",
                    "dotnet test",
                    "actual provider output",
                    if exit == 0 {
                        CommandStatus::Success
                    } else {
                        CommandStatus::Error
                    },
                    EngramCommandExit::Code(exit),
                )
                .unwrap();
        }
    }

    fn finish(
        &self,
        recorder: &mut SessionRecorder,
        exit: i64,
    ) -> (DeclaredRunReservation, TestRunRegistration, Value, Value) {
        self.end(recorder, exit);
        self.finished()
    }

    fn finished(&self) -> (DeclaredRunReservation, TestRunRegistration, Value, Value) {
        let start = self.wait();
        let inner = self.fixture.state.inner.lock().unwrap();
        let registration = inner.test_run_registrations[&start.run_id].clone();
        drop(inner);
        let request = serde_json::from_slice(
            &fs::read(PathBuf::from(&start.run_directory).join("request.json")).unwrap(),
        )
        .unwrap();
        let results = serde_json::from_slice(
            &fs::read(PathBuf::from(&start.run_directory).join("results.json")).unwrap(),
        )
        .unwrap();
        (start, registration, request, results)
    }

    fn artifact(&self, text: &str) {
        fs::create_dir_all(self.fixture.root.join(".tmp")).unwrap();
        fs::write(self.fixture.root.join(".tmp/test.trx"), text).unwrap();
    }

    fn later_prompt(&self) {
        let mut inner = self.fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&self.session).unwrap();
        inner.sessions[index].active_turn_generation += 1;
        inner.sessions[index].engram.declared_run_turn = None;
    }

    fn task_end(&self) {
        let work = claude_frame_work(
            &json!({"type":"system","subtype":"task_notification",
            "status":"completed","tool_use_id":"native-tool"}),
            false,
        );
        self.fixture
            .state
            .settle_claude_frame(&self.session, &self.token, &work);
    }

    fn original_results(
        &self,
        start: &DeclaredRunReservation,
    ) -> (TestRunRegistration, Value, Value) {
        let directory = PathBuf::from(&start.run_directory);
        let deadline = std::time::Instant::now() + crate::TEST_PHASE_DEADLOCK_GUARD;
        loop {
            let registration = self
                .fixture
                .state
                .inner
                .lock()
                .unwrap()
                .test_run_registrations
                .get(&start.run_id)
                .cloned();
            if let Some(registration) = registration
                && let TestRunJson::Parsed(results, _, _) =
                    test_run_read_json(&directory.join("results.json"))
                && engram_verification_record_results_are_terminal(&registration, &results)
            {
                let request =
                    serde_json::from_slice(&fs::read(directory.join("request.json")).unwrap())
                        .unwrap();
                return (registration, request, results);
            }
            assert!(
                std::time::Instant::now() < deadline,
                "original task terminal was not frozen"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn project_environment_native_original_start_uses_the_applied_record_selection() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for configured in [false, true] {
        let fixture = NativeFixture::new(
            if configured {
                "env-native-set"
            } else {
                "env-native-missing"
            },
            false,
        );
        let declaration = fixture.fixture.root.join(ENGRAM_DECLARATION_FILE);
        let text = fs::read_to_string(&declaration).unwrap();
        fs::write(
            &declaration,
            format!("{text}env = [\"SUPABASE_TEST_URL\"]\n"),
        )
        .unwrap();
        run_git_test_command(&fixture.fixture.root, &["add", ENGRAM_DECLARATION_FILE]);
        run_git_test_command(
            &fixture.fixture.root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--quiet",
                "--no-gpg-sign",
                "-m",
                "native environment fixture",
            ],
        );
        let workdir = fixture.fixture.root.parent().unwrap().join("w");
        run_git_test_command(
            &fixture.fixture.root,
            &[
                "worktree",
                "add",
                "--detach",
                workdir.to_str().unwrap(),
                "HEAD",
            ],
        );
        fs::write(workdir.join(ENGRAM_DECLARATION_FILE),
            "[[test]]\ncommand = \"dotnet test\"\nartifact = \".tmp/test.trx\"\nenv = [\"WORKDIR_ONLY\"]\n").unwrap();
        {
            let mut inner = fixture.fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.session).unwrap();
            inner.sessions[index].session.workdir = workdir.to_string_lossy().into_owned();
        }
        if configured {
            let mut inner = fixture.fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.session).unwrap();
            inner.sessions[index].project_environment = AppliedProjectEnvironment {
                values: ProjectEnvironment(BTreeMap::from([(
                    "SUPABASE_TEST_URL".to_owned(),
                    "native-fixture-value".to_owned(),
                )])),
            };
        }
        fixture.held_revision(4);
        fixture.prepare();
        let notice = fixture
            .fixture
            .state
            .project_environment_notice(&fixture.session);
        if configured {
            assert!(
                notice.is_none(),
                "notice must use named S0, not the session worktree"
            );
        } else {
            let notice = notice.unwrap();
            assert!(notice.contains("environment_missing:SUPABASE_TEST_URL"));
            assert!(!notice.contains("WORKDIR_ONLY"));
        }
        let mut recorder = fixture.recorder();
        fixture.start(&mut recorder);
        let original = fixture.wait();
        assert_eq!(
            original.declaration.environment_missing.is_none(),
            configured
        );
        fixture.start(&mut recorder);
        assert_eq!(
            fixture.wait(),
            original,
            "duplicate starts preserve the original environment decision"
        );
        fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
        let (_, registration, request, results) = fixture.finish(&mut recorder, 0);
        let body = engram_verification_record_body(&registration, &request, &results).unwrap();
        assert_eq!(
            body.run.outcome,
            if configured {
                EngramVerificationRecordRunOutcome::Passed
            } else {
                EngramVerificationRecordRunOutcome::Indeterminate
            }
        );
        assert!(
            !serde_json::to_string(&body)
                .unwrap()
                .contains("native-fixture-value")
        );
    }
}

#[test]
fn native_provider_starts_register_once_from_persisted_host_time_and_freeze_the_real_artifact() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for claude in [false, true] {
        let fixture = NativeFixture::new("native-host-original", claude);
        fixture.held_revision(4);
        fixture.prepare();
        let mut recorder = fixture.recorder();
        fixture.start(&mut recorder);
        let original = fixture.wait();
        fixture.start(&mut recorder);
        assert_eq!(fixture.wait(), original, "duplicate start cannot move R");
        assert_ne!(
            original.registered_at,
            original.provider_time["timestamp"].as_str().unwrap()
        );
        let persisted = load_state(fixture.fixture.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        assert_eq!(
            persisted.test_run_observations[&original.execution_id]
                .declared_native
                .as_ref(),
            Some(&original)
        );
        fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
        let (start, registration, request, results) = fixture.finish(&mut recorder, 0);
        assert_eq!(registration.registered_at, start.registered_at);
        assert_eq!(results["stages"][0]["started"], start.registered_at);
        if claude {
            assert!(
                results["exitCode"].is_null(),
                "reported success is not a numeric OS exit"
            );
            assert_eq!(results["stages"][0]["reportedSuccess"], true);
        }
        assert!(engram_verification_record_producer_refusal(&registration, &results).is_none());
        assert_eq!(request["observedCommand"], json!(["dotnet", "test"]));
        assert!(
            fs::canonicalize(&start.run_directory)
                .unwrap()
                .starts_with(fs::canonicalize(fixture.fixture.root.join(".git")).unwrap())
        );
        let body = engram_verification_record_body(&registration, &request, &results).unwrap();
        assert_eq!(body.run.outcome, EngramVerificationRecordRunOutcome::Passed);
        if claude {
            assert!(
                !test_run_results_are_terminal(&results),
                "generic launcher terminal rules stay numeric"
            );
            assert!(engram_verification_record_results_are_terminal(
                &registration,
                &results
            ));
            assert!(
                matches!(
                    engram_verification_record_prepare(&registration),
                    EngramVerificationRecordPrepared::Ready(_)
                ),
                "the real producer's pre-body terminal gate must admit the authenticated endpoint too"
            );
            let mut generic = registration.clone();
            generic.declared_native = None;
            assert!(
                !engram_verification_record_results_are_terminal(&generic, &results),
                "a terminal snapshot without its host reservation grants no exception"
            );
            assert!(
                matches!(
                    engram_verification_record_prepare(&generic),
                    EngramVerificationRecordPrepared::NotEligible
                ),
                "a generic registration cannot adopt source_id input"
            );
            let mut unreported = registration.clone();
            unreported
                .declared_terminal
                .as_mut()
                .unwrap()
                .reported_success = false;
            assert!(
                !engram_verification_record_results_are_terminal(&unreported, &results),
                "a result flag cannot supply the original ReportedSuccess"
            );
            let mut rewritten = results.clone();
            rewritten["stages"][0]["reportedSuccess"] = json!(false);
            assert!(
                !engram_verification_record_results_are_terminal(&registration, &rewritten),
                "the result must preserve reported success"
            );
            rewritten = results.clone();
            rewritten["laterValue"] = json!(true);
            assert!(
                !engram_verification_record_results_are_terminal(&registration, &rewritten),
                "even a shape-preserving rewrite must match the frozen results digest"
            );
        } else {
            assert!(test_run_results_are_terminal(&results));
        }
        fixture.artifact(include_str!("fixtures/declared-trx/fail.trx"));
        assert_eq!(
            engram_verification_record_body(&registration, &request, &results).unwrap(),
            body,
            "a later artifact cannot change the original frozen judgment"
        );
        let mut changed = results.clone();
        changed["exitCode"] = json!(1);
        assert!(engram_verification_record_body(&registration, &request, &changed).is_err());
        assert!(
            run_git_test_command_output(&fixture.fixture.root, &["status", "--porcelain"])
                .is_empty()
        );
    }
}

#[test]
fn native_display_uses_the_frozen_artifact_verdict_instead_of_a_successful_exit() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for claude in [false, true] {
        for artifact in ["pass", "fail", "missing", "stale", "zero", "malformed"] {
            let fixture =
                NativeFixture::new(&format!("native-display-{claude}-{artifact}"), claude);
            fixture.held_revision(4);
            if artifact == "stale" {
                fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
            }
            fixture.prepare();
            let mut recorder = fixture.recorder();
            fixture.start(&mut recorder);
            fixture.wait();
            match artifact {
                "pass" => fixture.artifact(include_str!("fixtures/declared-trx/pass.trx")),
                "fail" => fixture.artifact(include_str!("fixtures/declared-trx/fail.trx")),
                "zero" => fixture.artifact(include_str!("fixtures/declared-trx/zero.trx")),
                "malformed" => fixture.artifact("not a TRX result"),
                _ => {}
            }
            let (start, registration, request, results) = fixture.finish(&mut recorder, 0);
            let (expected_state, expected_outcome) = match artifact {
                "pass" => (
                    TestRunState::Passed,
                    EngramVerificationRecordRunOutcome::Passed,
                ),
                "fail" => (
                    TestRunState::Failed,
                    EngramVerificationRecordRunOutcome::Failed,
                ),
                _ => (
                    TestRunState::Unknown,
                    EngramVerificationRecordRunOutcome::Indeterminate,
                ),
            };
            assert_eq!(
                results["state"],
                serde_json::to_value(expected_state).unwrap(),
                "{claude}/{artifact}"
            );
            assert_eq!(results["stages"][0]["state"], results["state"]);
            assert_eq!(
                results["exitCode"],
                if claude { Value::Null } else { json!(0) },
                "the original provider endpoint is retained"
            );
            let body = engram_verification_record_body(&registration, &request, &results).unwrap();
            assert_eq!(body.run.outcome, expected_outcome, "{claude}/{artifact}");
            let inner = fixture.fixture.state.inner.lock().unwrap();
            assert_eq!(
                inner.test_run_observations[&start.execution_id]
                    .summary
                    .state,
                expected_state,
                "the original native row uses the same frozen verdict"
            );
            drop(inner);
            fixture.fixture.refresh(&[]);
            let indexed = fixture
                .fixture
                .state
                .test_run_summaries(None)
                .into_iter()
                .find(|summary| summary.run_id == start.run_id)
                .expect("the registered native run is indexed");
            if artifact != "pass" {
                assert_ne!(
                    indexed.state,
                    TestRunState::Passed,
                    "{claude}/{artifact}: no false passing card"
                );
            }
        }
    }
}

#[test]
fn native_worker_refusal_still_displays_the_original_end_artifact_verdict() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for claude in [false, true] {
        for artifact in ["pass", "fail", "missing", "stale", "zero", "malformed"] {
            let fixture = NativeFixture::new(
                &format!("native-refused-display-{claude}-{artifact}"),
                claude,
            );
            fixture.held_revision(4);
            if artifact == "stale" {
                fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
            }
            fixture.prepare();
            let mut recorder = fixture.recorder();
            let slots = fixture
                .fixture
                .state
                .inner
                .lock()
                .unwrap()
                .engram_turn_basis_captures_live
                .clone();
            slots.fetch_add(
                ENGRAM_TURN_BASIS_CAPTURE_LIMIT,
                std::sync::atomic::Ordering::SeqCst,
            );
            fixture.start(&mut recorder);
            slots.fetch_sub(
                ENGRAM_TURN_BASIS_CAPTURE_LIMIT,
                std::sync::atomic::Ordering::SeqCst,
            );
            let execution = fixture
                .execution()
                .expect("the original start has an observation");
            let start = {
                let job = execution.lock().unwrap();
                assert!(
                    job.failure
                        .as_deref()
                        .is_some_and(|failure| failure.contains("capacity unavailable"))
                );
                assert!(!job.worker_active);
                assert!(!job.registered);
                job.reservation.clone()
            };
            match artifact {
                "pass" => fixture.artifact(include_str!("fixtures/declared-trx/pass.trx")),
                "fail" => fixture.artifact(include_str!("fixtures/declared-trx/fail.trx")),
                "zero" => fixture.artifact(include_str!("fixtures/declared-trx/zero.trx")),
                "malformed" => fixture.artifact("not a TRX result"),
                _ => {}
            }
            fixture.end(&mut recorder, 0);
            let expected = match artifact {
                "pass" => TestRunState::Passed,
                "fail" => TestRunState::Failed,
                _ => TestRunState::Unknown,
            };
            let inner = fixture.fixture.state.inner.lock().unwrap();
            assert!(
                !inner.test_run_registrations.contains_key(&start.run_id),
                "capacity refusal earns no registration"
            );
            let row = &inner.test_run_observations[&start.execution_id];
            assert_eq!(row.summary.state, expected, "{claude}/{artifact}");
            assert_eq!(row.summary.exit_code, if claude { None } else { Some(0) });
            assert!(row.summary.ended_at.is_some());
            drop(inner);
            let indexed = fixture
                .fixture
                .state
                .test_run_summaries(None)
                .into_iter()
                .find(|summary| summary.run_id == start.execution_id)
                .expect("the original observation remains visible");
            assert_eq!(
                indexed.state, expected,
                "{claude}/{artifact}: the visible card follows the artifact"
            );
            assert!(
                execution.lock().unwrap().failure.is_some(),
                "artifact inspection cannot revive a failed worker"
            );
        }
    }
}

#[test]
fn native_end_freezes_artifact_before_a_blocked_registration_worker() {
    struct Release(Option<std::sync::mpsc::Sender<()>>);
    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }
    for failing_artifact in [false, true] {
        let fixture = NativeFixture::new("native-end-boundary", false);
        fixture.held_revision(4);
        fixture.prepare();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut release = Release(Some(release_tx));
        let release_rx = Arc::new(Mutex::new(release_rx));
        let blocked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        *fixture
            .fixture
            .state
            .capture_lifetime
            .before_capture
            .lock()
            .unwrap() = Some(Arc::new(move || {
            if std::thread::current().name() == Some("native-declared-run")
                && !blocked.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                // A durability wait may precede registration's source call.
                // Its worker must retain the source window after that wait.
                check_capture_cancellation_at(
                    std::time::Instant::now()
                        + TEST_RUN_REGISTRATION_DURABILITY_BUDGET
                        + ENGRAM_SOURCE_COMMAND_BUDGET / 2,
                )
                .expect("worker age cannot cancel a later native registration stage");
                ready_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(crate::TEST_PHASE_DEADLOCK_GUARD)
                    .unwrap();
            }
        }));
        let mut recorder = fixture.recorder();
        fixture.start(&mut recorder);
        ready_rx
            .recv_timeout(crate::TEST_PHASE_DEADLOCK_GUARD)
            .unwrap();
        if failing_artifact {
            fixture.artifact(include_str!("fixtures/declared-trx/fail.trx"));
        }
        recorder
            .command_completed_with_exit(
                "native-tool",
                "dotnet test",
                "original END",
                CommandStatus::Success,
                EngramCommandExit::Code(0),
            )
            .unwrap();
        // The worker has not registered or inspected anything yet. A later
        // passing artifact must not be borrowed by the completed invocation.
        fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
        release.0.take().unwrap().send(()).unwrap();
        let start = fixture.wait();
        let registration = fixture
            .fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_run_registrations[&start.run_id]
            .clone();
        let directory = PathBuf::from(&start.run_directory);
        let request =
            serde_json::from_slice(&fs::read(directory.join("request.json")).unwrap()).unwrap();
        let results =
            serde_json::from_slice(&fs::read(directory.join("results.json")).unwrap()).unwrap();
        let body = engram_verification_record_body(&registration, &request, &results).unwrap();
        assert_ne!(
            body.run.outcome,
            EngramVerificationRecordRunOutcome::Passed,
            "a later PASS cannot replace the artifact observed at original END"
        );
    }
}

#[test]
fn native_credit_compares_endpoints_independently_of_workspace_events() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for (restored, reported) in [(true, true), (false, true), (false, false)] {
        let fixture = NativeFixture::new("native-endpoint-only", false);
        let path = fixture.fixture.root.join("endpoint.txt");
        fs::write(&path, "original\n").unwrap();
        run_git_test_command(&fixture.fixture.root, &["add", "--", "endpoint.txt"]);
        run_git_test_command(
            &fixture.fixture.root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--quiet",
                "--no-gpg-sign",
                "-m",
                "native endpoint fixture",
            ],
        );
        fixture.held_revision(4);
        fixture.prepare();
        let mut recorder = fixture.recorder();
        fixture.start(&mut recorder);
        fixture.wait();
        fs::write(&path, "changed\n").unwrap();
        if reported {
            // Exercise the consumer with a normalized incoming event. The
            // fixture itself may live under target, which the watch adapter
            // deliberately excludes before producing such events.
            let changes =
                workspace_file_changes_from_path(&path, WorkspaceFileChangeKind::Modified, &[]);
            assert!(!changes.is_empty());
            fixture
                .fixture
                .state
                .note_engram_workspace_file_changes(&changes);
        }
        if restored {
            fs::write(&path, "original\n").unwrap();
        } else {
            fixture.record_change("endpoint.txt", " M");
        }
        fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
        let (_, registration, request, results) = fixture.finish(&mut recorder, 0);
        let body = engram_verification_record_body(&registration, &request, &results);
        if restored {
            assert_eq!(
                body.unwrap().run.outcome,
                EngramVerificationRecordRunOutcome::Passed,
                "equal clean endpoints accept a fully restored edit even when its event was observed"
            );
        } else {
            assert!(
                body.is_err(),
                "a changed full source_id refuses credit with or without an event"
            );
        }
    }
}

#[test]
fn native_no_snapshot_prior_activity_wrong_cwd_or_ambient_never_reserves() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for case in [
        "no-snapshot",
        "prior-read",
        "wrong-cwd",
        "ambient",
        "late-description",
    ] {
        let fixture = NativeFixture::new(case, false);
        let mut recorder = fixture.recorder();
        if case != "no-snapshot" {
            fixture.prepare();
        }
        if case == "prior-read" {
            recorder
                .native_tool_start(Some("earlier"), false, &json!({"name":"Read"}))
                .unwrap();
        }
        if case == "ambient" {
            recorder.recorder_state.native_tool_provenance = None;
            recorder.recorder_state.observation_provenance = EngramObservationProvenance::Ambient;
        }
        let mut frame = fixture.frame();
        if case == "wrong-cwd" {
            frame["cwd"] = json!(fixture.fixture.root.parent().unwrap().to_string_lossy());
        }
        if case == "late-description" {
            frame["command"] = Value::Null;
        }
        recorder
            .native_tool_start(Some("native-tool"), true, &frame)
            .unwrap();
        if case == "late-description" {
            recorder
                .native_tool_start(Some("native-tool"), true, &fixture.frame())
                .unwrap();
        }
        assert!(fixture.execution().is_none(), "{case}");
        assert!(
            fixture
                .fixture
                .state
                .inner
                .lock()
                .unwrap()
                .test_run_registrations
                .is_empty()
        );
    }
}

#[test]
fn native_credit_accepts_equal_dirty_source_ids_and_refuses_changed_dirty_content() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for changed in [false, true] {
        let fixture = NativeFixture::new("native-dirty-equality", false);
        fs::write(
            fixture.fixture.root.join("local.txt"),
            "local state before\n",
        )
        .unwrap();
        fixture.record_change("local.txt", "??");
        fixture.held_revision(4);
        fixture.prepare();
        let before = fixture.source_owner.identity();
        let mut recorder = fixture.recorder();
        fixture.start(&mut recorder);
        fixture.wait();
        if changed {
            fs::write(
                fixture.fixture.root.join("local.txt"),
                "different local state after\n",
            )
            .unwrap();
            fixture.record_change("local.txt", "??");
        }
        fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
        let (_, registration, request, results) = fixture.finish(&mut recorder, 0);
        assert_eq!(
            results["state"], "passed",
            "the original command result is retained"
        );
        let body = engram_verification_record_body(&registration, &request, &results);
        if changed {
            assert!(
                body.is_err(),
                "equal root commits do not stand in for different local content"
            );
        } else {
            let body = body.unwrap();
            assert_eq!(body.run.outcome, EngramVerificationRecordRunOutcome::Passed);
            let producers = engram_verification_record_producers(&registration.run_id, &body);
            assert_eq!(producers.len(), 1);
            let basis = producers[0].1.source_basis.as_ref().unwrap();
            assert_eq!(basis.source_id.as_ref(), Some(&before));
            assert!(basis.source_revision.is_empty());
            let wire = serde_json::to_value(body).unwrap();
            assert!(
                wire["input"] == json!({"kind":"source_id","source_id":before}),
                "the complete native input is retained"
            );
            assert!(
                wire["post_run"] == json!({"source_id":before}),
                "the complete native post-run identity is retained"
            );
            assert!(wire["post_run"].get("clean_before").is_none());
        }
    }
}

#[test]
fn native_credit_compares_the_complete_child_object_without_requiring_a_clean_child() {
    let fixture = NativeFixture::new("native-child-equality", false);
    let mut before = fixture.source_owner.identity();
    before.submodules = vec![EngramSourceIdSubmodule {
        path: "child/nested".into(),
        commit: before.commit.clone(),
        changes: vec![EngramSourceIdChange {
            path: "local.txt".into(),
            status: "??".into(),
            origin: None,
            blob: Some(before.tree.clone()),
            mode: Some(EngramSourceIdMode::Regular),
        }],
    }];
    fixture.source_owner.replace(before.clone());
    fixture.held_revision(4);
    fixture.prepare();
    let mut recorder = fixture.recorder();
    fixture.start(&mut recorder);
    fixture.wait();
    fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
    let (_, registration, request, results) = fixture.finish(&mut recorder, 0);
    let body = engram_verification_record_body(&registration, &request, &results).unwrap();
    assert!(
        serde_json::to_value(&body).unwrap()["input"]["source_id"] == json!(before),
        "the complete native input identity matches"
    );
    let producers = engram_verification_record_producers(&registration.run_id, &body);
    assert_eq!(producers.len(), 1);
    assert_eq!(
        producers[0]
            .1
            .source_basis
            .as_ref()
            .unwrap()
            .source_id
            .as_ref(),
        Some(&before)
    );
    for field in ["commit", "blob", "mode"] {
        let mut changed = registration.clone();
        let after = changed
            .declared_terminal
            .as_mut()
            .unwrap()
            .source
            .as_mut()
            .unwrap();
        let child = &mut after.source_id.submodules[0];
        match field {
            "commit" => child.commit = before.tree.clone(),
            "blob" => child.changes[0].blob = Some(before.commit.clone()),
            "mode" => child.changes[0].mode = Some(EngramSourceIdMode::Executable),
            _ => unreachable!(),
        }
        let mut changed_results = results.clone();
        changed_results["post_run"]["source_id"] = json!(after.source_id);
        changed.declared_terminal.as_mut().unwrap().results_digest =
            file_content_hash(&serde_json::to_vec_pretty(&changed_results).unwrap());
        assert!(
            engram_verification_record_body(&changed, &request, &changed_results).is_err(),
            "a changed child {field} cannot borrow the equal root commit"
        );
    }
}

#[test]
fn native_source_change_reason_reaches_withheld_record_and_turn_notice() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for kind in [
        "tracked_file",
        "untracked_file",
        "untracked_removed",
        "untracked_staged",
        "submodule_head",
        "many",
        "equal",
    ] {
        let fixture = NativeFixture::new("native-source-change-reason", false);
        let path = match kind {
            "submodule_head" => "child/nested",
            "many" => "entry-0000-é.txt",
            _ => "source.txt",
        };
        if matches!(kind, "untracked_removed" | "untracked_staged") {
            fs::write(fixture.fixture.root.join(path), "untracked before\n").unwrap();
            fixture.record_change(path, "??");
        }
        if kind == "tracked_file" {
            fs::write(fixture.fixture.root.join(path), "before\n").unwrap();
            run_git_test_command(&fixture.fixture.root, &["add", "--", path]);
            run_git_test_command(
                &fixture.fixture.root,
                &[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "commit",
                    "--quiet",
                    "--no-gpg-sign",
                    "-m",
                    "tracked source fixture",
                ],
            );
            fixture.source_owner.refresh_commit();
        }
        if kind == "submodule_head" {
            let mut source = fixture.source_owner.identity();
            source.submodules.push(EngramSourceIdSubmodule {
                path: path.into(),
                commit: "1".repeat(40),
                changes: Vec::new(),
            });
            fixture.source_owner.replace(source);
        }
        fixture.held_revision(4);
        fixture.prepare();
        let mut recorder = fixture.recorder();
        fixture.start(&mut recorder);
        fixture.wait();
        match kind {
            "tracked_file" | "untracked_file" => {
                fs::write(fixture.fixture.root.join(path), "after\n").unwrap();
                fixture.record_change(path, if kind == "untracked_file" { "??" } else { " M" });
            }
            "submodule_head" => {
                let mut source = fixture.source_owner.identity();
                source.submodules[0].commit = "2".repeat(40);
                fixture.source_owner.replace(source);
            }
            "untracked_removed" => {
                fs::remove_file(fixture.fixture.root.join(path)).unwrap();
                let mut source = fixture.source_owner.identity();
                source.changes.clear();
                fixture.source_owner.replace(source);
            }
            "untracked_staged" => fixture.record_change(path, "A "),
            "many" => {
                let mut source = fixture.source_owner.identity();
                source.changes = (0..128)
                    .map(|index| EngramSourceIdChange {
                        path: format!("entry-{index:04}-é.txt"),
                        status: "??".into(),
                        origin: None,
                        blob: Some(source.commit.clone()),
                        mode: Some(EngramSourceIdMode::Regular),
                    })
                    .collect();
                fixture.source_owner.replace(source);
            }
            _ => {}
        }
        fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
        let (_, registration, request, results) = fixture.finish(&mut recorder, 0);
        let prepared = engram_verification_record_prepare(&registration);
        if kind == "equal" {
            assert!(matches!(
                prepared,
                EngramVerificationRecordPrepared::Ready(_)
            ));
            assert_eq!(
                engram_verification_record_body(&registration, &request, &results)
                    .unwrap()
                    .run
                    .outcome,
                EngramVerificationRecordRunOutcome::Passed
            );
            let inner = fixture.fixture.state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
            assert!(
                !record
                    .engram
                    .pending_source_root_line
                    .as_deref()
                    .unwrap_or("")
                    .contains("source changed during the run")
            );
            continue;
        }
        assert_eq!(
            results["state"], "passed",
            "the artifact verdict stays factual"
        );
        assert_eq!(results["nativeEligible"], false);
        let EngramVerificationRecordPrepared::Pending(
            EngramVerificationRecordPending::SourceChanged { detail, changes },
        ) = prepared
        else {
            panic!("expected a specific source-change reason");
        };
        assert!(detail.contains("source changed during the run") && detail.contains(path));
        let expected_kind = if kind.starts_with("untracked") || kind == "many" {
            "untracked_file"
        } else {
            kind
        };
        assert!(changes.iter().any(|change| change.kind == expected_kind
            && change.path == path
            && change.before != change.after));
        let progress =
            engram_verification_record_progress(FsPath::new(&registration.run_directory)).unwrap();
        let progress = serde_json::to_value(progress).unwrap();
        assert_eq!(progress["reason"]["kind"], "source_changed");
        if kind == "many" {
            assert_eq!(
                progress["reason"]["changes"].as_array().unwrap().len(),
                128,
                "persist every changed entry"
            );
            assert!(
                progress["reason"]["detail"]
                    .as_str()
                    .unwrap()
                    .contains("entry-0127-é.txt")
            );
        }
        assert!(
            !PathBuf::from(&registration.run_directory)
                .join(ENGRAM_VERIFICATION_RECORD_BODY_FILE)
                .exists(),
            "source drift still emits no verification body"
        );
        let inner = fixture.fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
        let notice = record.engram.pending_source_root_line.as_deref().unwrap();
        assert!(
            notice.contains("source changed during the run") && notice.contains(path),
            "{notice}"
        );
        assert!(
            notice.len() <= ENGRAM_CHECK_CREDIT_LINE_MAX_BYTES,
            "bounded first credit line"
        );
        drop(inner);
        if kind == "many" {
            let mut inner = fixture.fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.session).unwrap();
            let engram = &mut inner.sessions[index].engram;
            engram.pending_source_root_line = None;
            let mut log = Vec::new();
            engram_publish_check_notice(
                &fixture.session,
                engram,
                format!("{ENGRAM_CHECK_CREDIT_LINE_PREFIX} {detail}"),
                &mut log,
            );
            assert!(
                String::from_utf8(log).unwrap().contains("entry-0127-é.txt"),
                "complete diagnostic reaches host log"
            );
            assert!(
                engram.pending_source_root_line.as_ref().unwrap().len()
                    <= ENGRAM_CHECK_CREDIT_LINE_MAX_BYTES
            );
        }
        let mut wrong_request = request.clone();
        wrong_request["nativeSourceId"] = Value::Null;
        let refusal =
            engram_verification_record_body(&registration, &wrong_request, &results).unwrap_err();
        assert!(
            refusal.contains("host reservation") && !refusal.contains("source changed"),
            "reservation mismatch remains distinct: <details withheld>"
        );
    }
}

#[test]
fn native_large_sources_are_preserved_and_genuine_frame_overflow_is_named() {
    let fixture = NativeFixture::new("native-source-overflow", false);
    let mut source = fixture.source_owner.identity();
    source.changes = (0..1600)
        .map(|index| EngramSourceIdChange {
            path: format!("local-{index:05}.txt"),
            status: "??".into(),
            origin: None,
            blob: Some(source.commit.clone()),
            mode: Some(EngramSourceIdMode::Regular),
        })
        .collect();
    fixture.source_owner.replace(source.clone());
    fixture.held_revision(4);
    fixture.prepare();
    let mut recorder = fixture.recorder();
    fixture.start(&mut recorder);
    fixture.wait();
    fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
    let (_, registration, request, results) = fixture.finish(&mut recorder, 0);
    assert!(
        results["input"]["source_id"] == json!(source),
        "the original complete evidence is retained"
    );
    assert_eq!(
        results["state"], "passed",
        "transport limits do not rewrite the original outcome"
    );
    let mut body = engram_verification_record_body(&registration, &request, &results).unwrap();
    let wire = serde_json::to_value(&body).unwrap();
    assert!(
        wire["input"] == json!({"kind":"source_id", "source_id":source}),
        "a large complete identity is neither truncated nor converted to a commit"
    );
    assert!(wire["post_run"]["source_id"] == json!(source));
    let frame = EngramControlRequest::VerificationRecord {
        operation_key: engram_verification_record_key(&registration.run_id),
        record: wire,
    };
    let encoded = encode_engram_control_request(&frame).unwrap();
    assert!(
        encoded.len() > ENGRAM_CONTROL_MAX_FRAME_BYTES,
        "the native fixture exceeds the historical frame capacity"
    );
    assert!(encoded.len() + 1 <= ACCEPTANCE_VERIFICATION_FRAME_MAX_BYTES);
    let EngramVerificationRecordPrepared::Ready(prepared) =
        engram_verification_record_prepare(&registration)
    else {
        panic!("the new private frame must prepare the complete native record");
    };
    assert_eq!(prepared.body, body);
    // The enlarged private channel still refuses a genuinely oversized
    // complete frame, including non-source fields.
    body.checks[0].summary = Some("x".repeat(ACCEPTANCE_VERIFICATION_FRAME_MAX_BYTES));
    let error = engram_verification_record_frame_bound(
        &body,
        &registration.run_id,
        "native_verification_frame_too_large",
    )
    .unwrap_err();
    assert!(
        error.starts_with("native_verification_frame_too_large:"),
        "<details withheld>"
    );
}

#[test]
fn native_artifact_failure_and_changed_source_cannot_inherit_a_pass() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for case in [
        "missing-artifact",
        "failing-artifact",
        "dirty-after",
        "owner-unavailable",
        "changed-root",
        "changed-claim",
    ] {
        let fixture = NativeFixture::new(case, false);
        fixture.prepare();
        let mut recorder = fixture.recorder();
        fixture.start(&mut recorder);
        fixture.wait();
        if case == "failing-artifact" {
            fixture.artifact(include_str!("fixtures/declared-trx/fail.trx"));
        } else if case != "missing-artifact" {
            fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
        }
        if case == "dirty-after" {
            fs::write(fixture.fixture.root.join("new-source"), "dirty").unwrap();
            fixture.record_change("new-source", "??");
        }
        if case == "owner-unavailable" {
            fixture
                .fixture
                .state
                .inner
                .lock()
                .unwrap()
                .test_declared_run_source = Some(Arc::new(|_, _, _| {
                anyhow::bail!("Engram source input_unavailable: scripted owner refused")
            }));
        }
        if case == "changed-root" {
            let mut inner = fixture.fixture.state.inner.lock().unwrap();
            inner.engram_work_source_roots[0].generation += 1;
        }
        if case == "changed-claim" {
            fixture.held_revision(4);
        }
        let (_, registration, request, results) = fixture.finish(&mut recorder, 0);
        let body = engram_verification_record_body(&registration, &request, &results);
        match case {
            "missing-artifact" => assert_eq!(
                body.unwrap().run.outcome,
                EngramVerificationRecordRunOutcome::Indeterminate
            ),
            "failing-artifact" => assert_eq!(
                body.unwrap().run.outcome,
                EngramVerificationRecordRunOutcome::Failed
            ),
            "owner-unavailable" => {
                assert_eq!(results["state"], "passed");
                assert!(results["post_run"]["source_id"].is_null());
                assert!(
                    results["nativeSourceUnavailableReason"]
                        .as_str()
                        .unwrap()
                        .contains("scripted owner refused")
                );
                assert!(body.unwrap_err().contains("scripted owner refused"));
            }
            _ => assert!(body.is_err(), "{case}"),
        }
    }
}

#[test]
fn native_metadata_admission_precedes_writes_and_refuses_a_redirect() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = NativeFixture::new("native-metadata-containment", false);
    fixture.prepare();
    let mut recorder = fixture.recorder();
    fixture.start(&mut recorder);
    let mut start = fixture.wait();
    let outside = fixture.fixture.root.join(".tmp/metadata-outside");
    fs::create_dir_all(&outside).unwrap();
    start.run_directory = test_run_display_path(&outside.join("review-runs").join(&start.run_id));
    assert!(declared_run_prepare_directory(&start).is_err());
    assert!(
        !outside.join("review-runs").exists(),
        "no directory may be created outside Git metadata"
    );
    let admin = fixture
        .fixture
        .root
        .join(".git/worktrees/native-containment");
    fs::create_dir_all(&admin).unwrap();
    let link = admin.join("review-runs");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    #[cfg(windows)]
    {
        let created = Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(link.to_string_lossy().replace('/', "\\"))
            .arg(outside.to_string_lossy().replace('/', "\\"))
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(
            created.status.success(),
            "{}",
            String::from_utf8_lossy(&created.stderr)
        );
    }
    start.run_directory = test_run_display_path(&link.join(&start.run_id));
    assert!(
        declared_run_prepare_directory(&start).is_err(),
        "resolved redirect is outside Git metadata"
    );
    assert_eq!(
        fs::read_dir(&outside).unwrap().count(),
        0,
        "neither request nor results was written through the redirect"
    );
}

#[test]
fn native_artifact_verdict_does_not_depend_on_provider_console_or_exit() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for case in [
        "cut",
        "no-preview",
        "background",
        "moved",
        "no-exit",
        "nonzero",
        "killed",
        "missing",
        "failing",
    ] {
        let fixture = NativeFixture::new(case, true);
        fixture.held_revision(4);
        fixture.prepare();
        let mut recorder = fixture.recorder();
        let mut frame = fixture.frame();
        if case == "background" {
            frame["input"]["run_in_background"] = json!(true);
        }
        register_claude_tool_use(
            &frame,
            &mut fixture.claude_state.lock().unwrap(),
            &mut recorder,
        )
        .unwrap();
        fixture.wait();
        match case {
            "missing" => {}
            "failing" => fixture.artifact(include_str!("fixtures/declared-trx/fail.trx")),
            _ => fixture.artifact(include_str!("fixtures/declared-trx/pass.trx")),
        }
        let mut result = json!({"stdout":"preview contains no test verdict"});
        if case == "cut" {
            result["persistedOutputPath"] = json!("/a/provider/path/that/must/not/be/opened");
            result["persistedOutputSize"] = json!(1_500_000);
        }
        if case == "no-preview" {
            result = json!({});
        }
        if case == "moved" {
            result["backgroundTaskId"] = json!("original-task");
        }
        if case == "killed" {
            result["interrupted"] = json!(true);
        }
        handle_claude_tool_result(
            &json!({"tool_use_result":result,"message":{"content":[{
                "type":"tool_result","tool_use_id":"native-tool",
                "is_error":matches!(case, "no-exit" | "nonzero" | "killed"),
                "content":if case == "nonzero" {"Exit code 1"} else {""},
            }]}}),
            &mut fixture.claude_state.lock().unwrap(),
            &mut recorder,
        )
        .unwrap();
        if matches!(case, "background" | "moved") {
            assert!(
                fixture.execution().unwrap().lock().unwrap().end.is_none(),
                "launch acknowledgement is not END"
            );
            let work = claude_frame_work(
                &json!({"type":"system","subtype":"task_notification",
                "status":"completed","tool_use_id":"native-tool"}),
                false,
            );
            fixture
                .fixture
                .state
                .settle_claude_frame(&fixture.session, &fixture.token, &work);
        }
        let (start, registration, request, results) = fixture.finished();
        let body = engram_verification_record_body(&registration, &request, &results).unwrap();
        let expected = match case {
            "killed" | "missing" => EngramVerificationRecordRunOutcome::Indeterminate,
            "failing" => EngramVerificationRecordRunOutcome::Failed,
            _ => EngramVerificationRecordRunOutcome::Passed,
        };
        assert_eq!(body.run.outcome, expected, "{case}");
        assert_eq!(
            results["interrupted"],
            case == "killed",
            "{case}: only actual interruption"
        );
        assert!(
            matches!(
                engram_verification_record_prepare(&registration),
                EngramVerificationRecordPrepared::Ready(_)
            ),
            "{case}"
        );
        let inner = fixture.fixture.state.inner.lock().unwrap();
        let row = &inner.test_run_observations[&start.execution_id];
        assert!(row.summary.ended_at.is_some(), "{case}");
        assert_eq!(
            row.summary.state == TestRunState::Passed,
            expected == EngramVerificationRecordRunOutcome::Passed,
            "{case}"
        );
    }
}

#[test]
fn native_background_task_uses_its_original_reservation_after_a_later_prompt() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for interrupted in [false, true] {
        let fixture = NativeFixture::new("native-task-end", true);
        fixture.held_revision(4);
        fixture.prepare();
        let mut recorder = fixture.recorder();
        fixture.start(&mut recorder);
        let start = fixture.wait();
        handle_claude_tool_result(&json!({"tool_use_result":{"backgroundTaskId":"task"},
            "message":{"content":[{"type":"tool_result","tool_use_id":"native-tool","is_error":false,"content":""}]}}),
            &mut fixture.claude_state.lock().unwrap(), &mut recorder).unwrap();
        assert!(
            fixture.execution().unwrap().lock().unwrap().end.is_none(),
            "launch alone has no artifact END"
        );
        {
            let mut inner = fixture.fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.session).unwrap();
            inner.sessions[index].active_turn_generation += 1;
            inner.sessions[index].engram.declared_run_turn = None;
        }
        fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
        let work = claude_frame_work(
            &json!({"type":"system","subtype":"task_notification",
            "status":if interrupted {"killed"} else {"completed"},"tool_use_id":"native-tool"}),
            false,
        );
        fixture
            .fixture
            .state
            .settle_claude_frame(&fixture.session, &fixture.token, &work);
        let deadline = std::time::Instant::now() + crate::TEST_PHASE_DEADLOCK_GUARD;
        let registration = loop {
            let current = fixture
                .fixture
                .state
                .inner
                .lock()
                .unwrap()
                .test_run_registrations[&start.run_id]
                .clone();
            if let TestRunJson::Parsed(results, _, _) =
                test_run_read_json(&PathBuf::from(&start.run_directory).join("results.json"))
                && engram_verification_record_results_are_terminal(&current, &results)
            {
                break current;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "original task terminal was not frozen"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        let request = serde_json::from_slice(
            &fs::read(PathBuf::from(&start.run_directory).join("request.json")).unwrap(),
        )
        .unwrap();
        let results = serde_json::from_slice(
            &fs::read(PathBuf::from(&start.run_directory).join("results.json")).unwrap(),
        )
        .unwrap();
        let body = engram_verification_record_body(&registration, &request, &results).unwrap();
        assert_eq!(
            body.run.outcome,
            if interrupted {
                EngramVerificationRecordRunOutcome::Indeterminate
            } else {
                EngramVerificationRecordRunOutcome::Passed
            }
        );
        assert_eq!(registration.declared_native.as_ref(), Some(&start));
        let inner = fixture.fixture.state.inner.lock().unwrap();
        let row = &inner.test_run_observations[&start.execution_id];
        assert!(
            row.summary.error.is_none(),
            "a valid native artifact must not receive the no-verdict task notice"
        );
        assert_eq!(row.summary.state == TestRunState::Passed, !interrupted);
    }
}

#[test]
fn native_background_ack_with_a_complete_artifact_waits_for_after_run_source() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for background in [false, true] {
        for changed in [false, true] {
            let fixture = NativeFixture::new("native-background-after-source", true);
            fixture.held_revision(4);
            fixture.prepare();
            let captures = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let observed = captures.clone();
            let owner = fixture.source_owner.clone();
            fixture
                .fixture
                .state
                .inner
                .lock()
                .unwrap()
                .test_declared_run_source = Some(Arc::new(move |binding, _, deadline| {
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                owner.source(binding, deadline)
            }));
            let mut recorder = fixture.recorder();
            let mut frame = fixture.frame();
            if background {
                frame["input"]["run_in_background"] = json!(true);
            }
            register_claude_tool_use(
                &frame,
                &mut fixture.claude_state.lock().unwrap(),
                &mut recorder,
            )
            .unwrap();
            let start = fixture.wait();
            fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
            handle_claude_tool_result(&json!({"tool_use_result":if background { json!({}) } else { json!({"backgroundTaskId":"task"}) },
                "message":{"content":[{"type":"tool_result","tool_use_id":"native-tool","is_error":false,"content":""}]}}),
                &mut fixture.claude_state.lock().unwrap(), &mut recorder).unwrap();
            assert_eq!(
                captures.load(std::sync::atomic::Ordering::SeqCst),
                0,
                "launch ACK cannot capture after-run source"
            );
            assert!(fixture.execution().unwrap().lock().unwrap().end.is_none());
            assert!(
                fixture
                    .fixture
                    .state
                    .inner
                    .lock()
                    .unwrap()
                    .test_run_registrations[&start.run_id]
                    .declared_terminal
                    .is_none(),
                "complete artifact alone cannot publish a terminal"
            );
            if changed {
                fs::write(
                    fixture.fixture.root.join("during-background.txt"),
                    "changed after launch ACK\n",
                )
                .unwrap();
                fixture.record_change("during-background.txt", "??");
            }
            fixture.task_end();
            let (registration, request, results) = fixture.original_results(&start);
            assert_eq!(captures.load(std::sync::atomic::Ordering::SeqCst), 1);
            let body = engram_verification_record_body(&registration, &request, &results);
            if changed {
                assert!(
                    body.is_err(),
                    "source changed before original task completion"
                );
            } else {
                assert_eq!(
                    body.unwrap().run.outcome,
                    EngramVerificationRecordRunOutcome::Passed
                );
            }
        }
    }
}

#[test]
fn native_task_completion_after_a_later_prompt_preserves_original_refusal_and_failure() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for capacity_failure in [false, true] {
        let fixture = NativeFixture::new("native-task-original-refusal", true);
        fixture.held_revision(4);
        fixture.prepare();
        let slots = fixture
            .fixture
            .state
            .inner
            .lock()
            .unwrap()
            .engram_turn_basis_captures_live
            .clone();
        let mut recorder = fixture.recorder();
        if capacity_failure {
            slots.fetch_add(
                ENGRAM_TURN_BASIS_CAPTURE_LIMIT,
                std::sync::atomic::Ordering::SeqCst,
            );
        }
        fixture.start(&mut recorder);
        if capacity_failure {
            slots.fetch_sub(
                ENGRAM_TURN_BASIS_CAPTURE_LIMIT,
                std::sync::atomic::Ordering::SeqCst,
            );
        } else {
            fixture.wait();
        }
        let execution = fixture.execution().unwrap();
        let start = execution.lock().unwrap().reservation.clone();
        if !capacity_failure {
            let mut contradictory = fixture.frame();
            contradictory["input"]["command"] = json!("dotnet test --different-command");
            recorder
                .native_tool_start(Some("native-tool"), true, &contradictory)
                .unwrap();
        }
        handle_claude_tool_result(&json!({"tool_use_result":{"backgroundTaskId":"task"},
            "message":{"content":[{"type":"tool_result","tool_use_id":"native-tool","is_error":false,"content":""}]}}),
            &mut fixture.claude_state.lock().unwrap(), &mut recorder).unwrap();
        fixture.later_prompt();
        fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
        fixture.task_end();
        let job = execution.lock().unwrap();
        assert!(if capacity_failure {
            job.failure.is_some()
        } else {
            job.verification_refusal.is_some()
        });
        assert!(
            !job.worker_active,
            "task END must not start a replacement worker for a refused execution"
        );
        drop(job);
        let inner = fixture.fixture.state.inner.lock().unwrap();
        let row = &inner.test_run_observations[&start.execution_id];
        assert!(
            row.summary.ended_at.is_some(),
            "the actual task END remains observed"
        );
        assert!(
            inner
                .test_run_registrations
                .get(&start.run_id)
                .is_none_or(|registration| registration.declared_terminal.is_none()),
            "a later prompt cannot revive native credit"
        );
        if capacity_failure {
            assert!(!inner.test_run_registrations.contains_key(&start.run_id));
        }
    }
}

#[test]
fn native_task_end_during_pending_registration_keeps_the_original_worker_across_prompts() {
    struct Release(Option<std::sync::mpsc::Sender<()>>);
    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }
    let fixture = NativeFixture::new("native-task-pending-registration", true);
    fixture.held_revision(4);
    fixture.prepare();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let mut release = Release(Some(release_tx));
    let release_rx = Arc::new(Mutex::new(release_rx));
    let blocked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    *fixture
        .fixture
        .state
        .capture_lifetime
        .before_capture
        .lock()
        .unwrap() = Some(Arc::new(move || {
        if std::thread::current().name() == Some("native-declared-run")
            && !blocked.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            ready_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(crate::TEST_PHASE_DEADLOCK_GUARD)
                .unwrap();
        }
    }));
    let mut recorder = fixture.recorder();
    fixture.start(&mut recorder);
    ready_rx
        .recv_timeout(crate::TEST_PHASE_DEADLOCK_GUARD)
        .unwrap();
    let execution = fixture.execution().unwrap();
    let start = execution.lock().unwrap().reservation.clone();
    fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
    handle_claude_tool_result(&json!({"tool_use_result":{"backgroundTaskId":"task"},
        "message":{"content":[{"type":"tool_result","tool_use_id":"native-tool","is_error":false,"content":""}]}}),
        &mut fixture.claude_state.lock().unwrap(), &mut recorder).unwrap();
    fixture.later_prompt();
    fixture.task_end();
    {
        let job = execution.lock().unwrap();
        assert!(
            job.end.is_some(),
            "original task END freezes while registration waits"
        );
        assert!(job.worker_active && !job.registered);
    }
    assert!(
        !fixture
            .fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_run_registrations
            .contains_key(&start.run_id)
    );
    release.0.take().unwrap().send(()).unwrap();
    let (registration, request, results) = fixture.original_results(&start);
    assert_eq!(registration.declared_native.as_ref(), Some(&start));
    assert_eq!(
        engram_verification_record_body(&registration, &request, &results)
            .unwrap()
            .run
            .outcome,
        EngramVerificationRecordRunOutcome::Passed
    );
}

#[test]
fn native_declared_known_runners_and_prebuilt_options_use_their_artifact() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for command in [
        "cargo test",
        "dotnet test --no-build",
        "dotnet test bin/Tests.dll",
    ] {
        let fixture = NativeFixture::new("native-declared-options", false);
        fs::write(
            fixture.fixture.root.join(ENGRAM_DECLARATION_FILE),
            format!("[[test]]\ncommand = {command:?}\ncwd = \".\"\nartifact = \".tmp/test.trx\"\n"),
        )
        .unwrap();
        run_git_test_command(
            &fixture.fixture.root,
            &["add", "--", ENGRAM_DECLARATION_FILE],
        );
        run_git_test_command(
            &fixture.fixture.root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--quiet",
                "--no-gpg-sign",
                "-m",
                "declared runner fixture",
            ],
        );
        fixture.held_revision(4);
        fixture.prepare();
        let mut recorder = fixture.recorder();
        let mut frame = fixture.frame();
        frame["command"] = json!(command);
        recorder
            .native_tool_start(Some("native-tool"), true, &frame)
            .unwrap();
        fixture.wait();
        fixture.artifact(include_str!("fixtures/declared-trx/pass.trx"));
        recorder
            .command_completed_with_exit(
                "native-tool",
                command,
                "",
                CommandStatus::Success,
                EngramCommandExit::Code(0),
            )
            .unwrap();
        let (_, registration, request, results) = fixture.finished();
        assert_eq!(
            engram_verification_record_body(&registration, &request, &results)
                .unwrap()
                .run
                .outcome,
            EngramVerificationRecordRunOutcome::Passed,
            "{command}"
        );
    }
}

#[test]
fn a_valid_native_artifact_larger_than_four_mib_retains_its_verdict_and_counts() {
    let fixture = NativeFixture::new("native-large-artifact", false);
    fixture.held_revision(4);
    fixture.prepare();
    let mut recorder = fixture.recorder();
    fixture.start(&mut recorder);
    fixture.wait();
    let artifact = include_str!("fixtures/declared-trx/pass.trx").replace(
        "</TestRun>",
        &format!("<!--{}--></TestRun>", "x".repeat(5 * 1024 * 1024)),
    );
    assert!(artifact.len() > 4 * 1024 * 1024);
    fixture.artifact(&artifact);
    let (_, registration, request, results) = fixture.finish(&mut recorder, 0);
    let body = engram_verification_record_body(&registration, &request, &results).unwrap();
    assert_eq!(body.run.outcome, EngramVerificationRecordRunOutcome::Passed);
    assert!(
        body.checks[0]
            .counts
            .as_ref()
            .is_some_and(|counts| counts["passed"] > 0)
    );
}
