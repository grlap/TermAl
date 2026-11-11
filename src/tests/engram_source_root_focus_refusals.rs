//! Prospective refresh is not a new name or permission to bypass a raced root.
use super::*;

struct FocusFixture {
    turn: EOCheckedTurn,
    transport: Arc<FocusSelectingTransport>,
    first: EngramControlWorkBinding,
    second: EngramControlWorkBinding,
    held: EngramHeldClaims,
}

fn fixture(label: &str) -> FocusFixture {
    let (turn, first_path) = confirmation_root(label);
    let transport = FocusSelectingTransport::new(turn.transport.clone());
    turn.state.install_control_test_transport(transport.clone());
    let mut held = naming_claim(label);
    let first = held.items[0].control_binding.clone().unwrap();
    let second = test_control_work_binding(&format!("{label}-second"), 1);
    held.items.push(second_claim(&second));
    request_naming(&turn, label, Some(&first_path), vec![held.clone()]).unwrap();
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
    turn.transport.register_named_root_run(&second);
    focus(&turn, &held, &second);
    turn.state
        .name_engram_source_root(
            &turn.session_id,
            EngramSourceRootRequest {
                work: "w-second".to_owned(),
                path: Some(Some(second_path.to_string_lossy().into_owned())),
            },
        )
        .unwrap();
    // These controls exercise ambiguous/released old scope while validating
    // the prospective root independently. A still-held old claim has its own
    // positive continuity test in the parent module.
    held.items.retain(|claim| claim.claim_id != second.claim_id);
    let fixture = FocusFixture {
        turn,
        transport,
        first,
        second,
        held,
    };
    assert_focused_name_confirmed(&fixture);
    fixture
}

fn assert_focused_name_confirmed(f: &FocusFixture) {
    let inner = f.turn.state.inner.lock().unwrap();
    let store = inner
        .projects
        .iter()
        .find_map(|project| {
            project
                .engram
                .as_ref()
                .and_then(|settings| settings.authority_store_key.as_ref())
        })
        .unwrap();
    let root = engram_work_source_root_for_claim(
        &inner.engram_work_source_roots,
        store,
        &f.first.work_id,
        &f.first.claim_id,
    )
    .unwrap();
    let claim = AcceptanceEvaluationSourceClaim {
        work_id: f.first.work_id.clone(),
        claim_id: f.first.claim_id.clone(),
        named_generation_at_request: Some(root.generation),
    };
    assert!(
        !engram_authority_work_unresolved(&inner, store, &f.first.work_id),
        "a refused prospective refresh must leave the settled work confirmed"
    );
    assert!(
        !acceptance_evaluation_claim_unconfirmed_locked(
            &inner,
            &f.turn.session_id,
            store,
            Some(&claim)
        ),
        "the existing acknowledged name must still pass evaluation's authority fence"
    );
}

fn assert_not_refreshed(f: &FocusFixture, turn_ref: &str) {
    assert_eq!(
        f.turn.record(|record| record.engram.work_binding.clone()),
        Some(f.second.clone())
    );
    assert!(!f.turn.transport.requests().iter().any(
        |sent| sent.request["idempotency_key"] == format!("termal-source-sighting:{turn_ref}")
    ));
    let notice = f
        .turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .unwrap();
    assert!(notice.contains("No future source root was installed"));
    assert!(notice.contains("Name that work's root for a later turn"));
    assert!(!notice.contains("refreshed for a later opening"));
    assert_private_focus_notice(&notice, &[&f.first, &f.second]);
    f.turn
        .record(|record| assert_private_focus_system_notices(record, &[&f.first, &f.second]));
    assert_focused_name_confirmed(f);
}

fn held_fixture(label: &str) -> FocusFixture {
    let mut f = fixture(label);
    f.held.items.push(second_claim(&f.second));
    // The producer accepts B's historical observations while focus moves to A.
    f.turn
        .transport
        .enable_named_roots(&f.turn.session_id, &f.second);
    focus(&f.turn, &f.held, &f.second);
    f
}

#[test]
fn a_refused_refresh_keeps_the_held_old_report_and_notices_the_next_opening() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let f = held_fixture("held-focus-refresh-refused");
    f.turn
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .root_read_failures
        .insert(f.first.claim_id.clone());
    let names = f
        .turn
        .state
        .inner
        .lock()
        .unwrap()
        .engram_work_source_roots
        .clone();
    let original_root = f.turn.record(|record| record.engram.named_root.clone());
    f.turn.begin("The held old claim still owns this opening.");
    let turn_ref = observed_turn_ref(&f.turn);
    focus(&f.turn, &f.held, &f.first);
    f.turn.end();
    settle_close(&f.turn);
    f.turn.record(|record| {
        assert_eq!(record.engram.work_binding.as_ref(), Some(&f.second));
        assert_eq!(record.engram.named_root, original_root);
        assert!(record.engram.unadmitted_closing_jobs.is_empty());
        let report = record
            .engram
            .unadmitted_reports
            .iter()
            .find(|report| report.host_turn_ref == turn_ref)
            .unwrap();
        assert!(report.envelopes.iter().all(|envelope| matches!(
            envelope.delivery,
            EngramRuntimeObservationDelivery::Recorded { .. }
        )));
        let notice = record.engram.pending_source_root_line.as_ref().unwrap();
        assert!(notice.contains("report is retained under its original item"));
        assert!(notice.contains("No future source root was installed"));
        assert!(notice.contains("Name or confirm the focused item's root"));
        assert!(!notice.contains("uncaptured"));
        assert_private_focus_notice(notice, &[&f.first, &f.second]);
        assert_private_focus_system_notices(record, &[&f.first, &f.second]);
    });
    assert_eq!(
        f.turn.state.inner.lock().unwrap().engram_work_source_roots,
        names
    );
    let requests = f.turn.transport.requests();
    let sighting = requests
        .iter()
        .find(|sent| {
            sent.request["idempotency_key"] == format!("termal-source-sighting:{turn_ref}")
        })
        .unwrap();
    assert_eq!(sighting.request["binding"], json!(f.second));
    assert_focused_name_confirmed(&f);
    // The remedy reaches the agent's next prompt; it is not only a log line.
    assert!(
        f.turn
            .begin_prompt("Read the prospective association refusal before continuing.")
            .prompt
            .contains("Name or confirm the focused item's root")
    );
    f.turn.end();
    settle_close(&f.turn);
}

struct FocusReadGate(Arc<(Mutex<bool>, Condvar)>);

impl Drop for FocusReadGate {
    fn drop(&mut self) {
        phase_sync::set_gate(&self.0, true);
    }
}

#[test]
fn retirement_does_not_wait_for_the_prospective_read_or_publish_its_late_result() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let f = held_fixture("held-focus-refresh-retired");
    let original_root = f.turn.record(|record| record.engram.named_root.clone());
    f.turn
        .begin("Retain the old report before prospective work is cancelled.");
    let original_notice = f
        .turn
        .record(|record| record.engram.pending_source_root_line.clone());
    let turn_ref = observed_turn_ref(&f.turn);
    let gate = FocusReadGate(Arc::new((Mutex::new(false), Condvar::new())));
    let blocked = gate.0.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let weak_transport = Arc::downgrade(&f.transport);
    // The original B proof must finish. Block only the subsequent A proof,
    // after its old report is retained and the install guard must be gone.
    *f.transport.root_read_hook.lock().unwrap() = Some(Box::new(move || {
        let transport = weak_transport.upgrade().unwrap();
        *transport.root_read_hook.lock().unwrap() = Some(Box::new(move || {
            entered_tx.send(()).unwrap();
            phase_sync::wait_for_gate(&blocked, true);
        }));
    }));
    focus(&f.turn, &f.held, &f.first);
    f.turn.end();
    receive(
        &entered_rx,
        "prospective proof is blocked after the original report install",
    );
    f.turn.record(|record| {
        assert!(
            record
                .engram
                .unadmitted_reports
                .iter()
                .any(|report| report.host_turn_ref == turn_ref)
        )
    });
    let lifetime = f.turn.state.runtime_observation_lifetime.clone();
    let (joined_tx, joined_rx) = mpsc::channel();
    let retiring = std::thread::spawn(move || {
        lifetime.stop_and_join();
        joined_tx.send(()).unwrap();
    });
    receive(
        &joined_rx,
        "retirement drains installations without waiting for the remote read",
    );
    retiring.join().unwrap();
    drop(gate);
    let guard = phase_sync::PollGuard::new();
    while f
        .turn
        .record(|record| record.engram.unadmitted_worker_running)
    {
        guard.wait("the stopped worker discards the late proof");
    }
    f.turn.record(|record| {
        assert_eq!(record.engram.work_binding.as_ref(), Some(&f.second));
        assert_eq!(record.engram.named_root, original_root);
        assert_eq!(record.engram.pending_source_root_line, original_notice);
        assert!(record.engram.unadmitted_closing_jobs.is_empty());
        assert!(
            record
                .engram
                .unadmitted_reports
                .iter()
                .any(|report| report.host_turn_ref == turn_ref)
        );
    });
}

#[test]
fn a_focus_switch_cannot_restore_lost_claim_or_opening_authority() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for change in [
        "released",
        "omitted",
        "expired",
        "revision",
        "fence",
        "run",
        "root-ended",
        "unconfirmed-opening",
    ] {
        let mut f = fixture(&format!("focus-original-refusal-{change}"));
        if change == "omitted" {
            f.held.omitted = 1;
        } else if change != "released" {
            let mut original = second_claim(&f.second);
            match change {
                "expired" => original.control_binding = None,
                "revision" => original.control_binding.as_mut().unwrap().work_revision += 1,
                "fence" => {
                    original.claim_fence += 1;
                    original.control_binding.as_mut().unwrap().claim_fence += 1;
                }
                "run" => original
                    .control_binding
                    .as_mut()
                    .unwrap()
                    .run_id
                    .push_str("-new"),
                _ => {}
            }
            f.held.items.push(original);
        }
        let original_root = f.turn.record(|record| record.engram.named_root.clone());
        if change == "unconfirmed-opening" {
            f.turn
                .state
                .inner
                .lock()
                .unwrap()
                .session_mut(&f.turn.session_id)
                .unwrap()
                .engram
                .named_root = Some(EngramNamedRootState::Unknown);
        }
        f.turn
            .begin("A focus change cannot supply authority missing at this opening or close.");
        let turn_ref = observed_turn_ref(&f.turn);
        if change == "unconfirmed-opening" {
            assert!(f.turn.record(|record| matches!(
                &record.engram.observed_opening().unwrap().provenance,
                EngramRootCapture::Unconfirmed
            )));
            // A later repair cannot upgrade the opening already delivered.
            f.turn
                .state
                .inner
                .lock()
                .unwrap()
                .session_mut(&f.turn.session_id)
                .unwrap()
                .engram
                .named_root = original_root;
        }
        if change == "root-ended" {
            let mut roots = f.turn.transport.named_roots.lock().unwrap();
            let roots = roots.as_mut().unwrap();
            let proof = roots
                .request(
                    &f.turn.session_id,
                    &EngramControlRequest::NamedRootRead {
                        run_id: f.second.run_id.clone(),
                        claim_id: f.second.claim_id.clone(),
                    },
                )
                .unwrap()
                .unwrap();
            roots
                .request(
                    &f.turn.session_id,
                    &EngramControlRequest::NamedRootBind {
                        claim_id: f.second.claim_id.clone(),
                        claim_fence: f.second.claim_fence,
                        workspace_id: proof["named_root"]["workspace_id"]
                            .as_str()
                            .unwrap()
                            .to_owned(),
                        generation: proof["named_root"]["generation"].as_i64().unwrap(),
                        named_at: proof["named_root"]["named_at"].as_str().unwrap().to_owned(),
                        kind: EngramNamedRootKind::Ended,
                        end_reason: Some(EngramNamedRootEndReason::ExplicitClear),
                        idempotency_key: "fixture-end-original-root".to_owned(),
                    },
                )
                .unwrap()
                .unwrap();
        }
        focus(&f.turn, &f.held, &f.first);
        f.turn.end();
        settle_close(&f.turn);
        f.turn.record(|record| {
            assert!(
                record
                    .engram
                    .unadmitted_closing_jobs
                    .iter()
                    .any(|job| job.trace.host_turn_ref == turn_ref
                        && job.terminal_failure.is_some()
                        && job.scope.binding.as_ref() == Some(&f.second)),
                "{change}"
            );
            assert!(
                !record
                    .engram
                    .unadmitted_reports
                    .iter()
                    .any(|report| report.host_turn_ref == turn_ref),
                "{change}"
            );
            assert!(
                record
                    .session
                    .messages
                    .iter()
                    .any(|message| matches!(message,
                Message::Text { author: Author::System, text, .. }
                    if text.contains("runtime_observation_scope_changed"))),
                "{change}"
            );
        });
        assert!(
            !f.turn
                .transport
                .requests()
                .iter()
                .any(|sent| sent.request["idempotency_key"]
                    == format!("termal-source-sighting:{turn_ref}")),
            "{change}"
        );
    }
}

#[test]
fn an_unavailable_or_changed_canonical_root_does_not_refresh_focus() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for failure in ["unavailable", "generation", "authority"] {
        let f = fixture(&format!("focus-proof-refusal-{failure}"));
        {
            let mut roots = f.turn.transport.named_roots.lock().unwrap();
            let roots = roots.as_mut().unwrap();
            if failure != "unavailable" {
                let mut proof = roots
                    .request(
                        &f.turn.session_id,
                        &EngramControlRequest::NamedRootRead {
                            run_id: f.first.run_id.clone(),
                            claim_id: f.first.claim_id.clone(),
                        },
                    )
                    .unwrap()
                    .unwrap();
                if failure == "generation" {
                    let generation = proof["named_root"]["generation"].as_i64().unwrap() + 1;
                    let position = proof["read_cut"]["position"].as_i64().unwrap() + 1;
                    proof["named_root"]["generation"] = json!(generation);
                    proof["latest_event"]["generation"] = json!(generation);
                    proof["latest_event"]["event"] =
                        json!(sha256_hex(b"a newer canonical naming event"));
                    proof["latest_event"]["position"]["position"] = json!(position);
                    proof["read_cut"]["position"] = json!(position);
                } else {
                    proof["work_id"] = json!("another-work");
                }
                roots.root_reads.insert(f.first.claim_id.clone(), proof);
            } else {
                roots.root_read_failures.insert(f.first.claim_id.clone());
            }
        }
        let names = f
            .turn
            .state
            .inner
            .lock()
            .unwrap()
            .engram_work_source_roots
            .clone();
        focus(&f.turn, &f.held, &f.first);
        f.turn
            .begin("The source proof must agree with the acknowledged local name.");
        let turn_ref = observed_turn_ref(&f.turn);
        let before = f.turn.transport.requests().len();
        f.turn.end();
        settle_close(&f.turn);
        assert_not_refreshed(&f, &turn_ref);
        assert_eq!(
            f.turn.state.inner.lock().unwrap().engram_work_source_roots,
            names
        );
        let reads = f
            .turn
            .transport
            .requests()
            .into_iter()
            .skip(before)
            .filter(|sent| sent.request["operation"] == "named_root_read")
            .count();
        assert_eq!(
            reads, 1,
            "the refusal gets one bounded proof read, not a recovery loop"
        );
    }
}

#[test]
fn a_workdir_change_during_the_proof_read_cannot_publish_a_future_root() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let f = fixture("focus-local-race");
    focus(&f.turn, &f.held, &f.first);
    f.turn
        .begin("Keep this scope while the canonical read is in flight.");
    let turn_ref = observed_turn_ref(&f.turn);
    let state = f.turn.state.clone();
    let session = f.turn.session_id.clone();
    let moved = f.turn.root.join("moved-workdir");
    fs::create_dir_all(&moved).unwrap();
    *f.transport.root_read_hook.lock().unwrap() = Some(Box::new(move || {
        state
            .inner
            .lock()
            .unwrap()
            .session_mut(&session)
            .unwrap()
            .session
            .workdir = moved.to_string_lossy().into_owned();
    }));
    f.turn.end();
    settle_close(&f.turn);
    assert_not_refreshed(&f, &turn_ref);
}

#[test]
fn a_new_opening_during_the_proof_read_keeps_its_original_scope() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let f = fixture("focus-opening-race");
    focus(&f.turn, &f.held, &f.first);
    f.turn
        .begin("Finish before the next opening races the root proof.");
    let turn_ref = observed_turn_ref(&f.turn);
    let state = f.turn.state.clone();
    let session = f.turn.session_id.clone();
    let (opened_tx, opened_rx) = mpsc::channel();
    *f.transport.root_read_hook.lock().unwrap() = Some(Box::new(move || {
        let dispatch = match state
            .dispatch_turn(
                &session,
                SendMessageRequest {
                    text: "An already delivered opening cannot inherit later authority.".to_owned(),
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
            DispatchTurnResult::Queued => panic!("the completed turn must leave an idle session"),
        };
        assert!(!deliver_turn_dispatch(&state, dispatch).is_err());
        opened_tx.send(()).unwrap();
    }));
    f.turn.end();
    receive(
        &opened_rx,
        "the next provider prompt is delivered before the proof returns",
    );
    let guard = phase_sync::PollGuard::new();
    while !f.turn.record(|record| {
        record
            .engram
            .unadmitted_closing_jobs
            .iter()
            .any(|job| job.trace.host_turn_ref == turn_ref && job.terminal_failure.is_some())
    }) {
        guard.wait("the earlier refusal is installed without changing the active opening");
    }
    assert_not_refreshed(&f, &turn_ref);
    f.turn
        .record(|record| assert_eq!(record.engram.observed_opening().unwrap().binding, f.second));
    f.turn.end();
    settle_close(&f.turn);
}

#[test]
fn a_naming_owner_during_the_proof_read_keeps_its_binding_and_settled_authority() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let f = fixture("focus-naming-race");
    focus(&f.turn, &f.held, &f.first);
    f.turn
        .begin("A naming owner must keep its claim association.");
    let turn_ref = observed_turn_ref(&f.turn);
    let state = f.turn.state.inner.clone();
    let session = f.turn.session_id.clone();
    *f.transport.root_read_hook.lock().unwrap() = Some(Box::new(move || {
        state
            .lock()
            .unwrap()
            .session_mut(&session)
            .unwrap()
            .engram
            .source_root_bind_owner = Some(Uuid::new_v4());
    }));
    let state = f.turn.state.inner.clone();
    let session = f.turn.session_id.clone();
    let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let saw_owner = released.clone();
    // Release only at installation, after the prospective-refresh fence has
    // judged the owner. The ordinary closing job can then publish its refusal.
    *f.turn
        .state
        .runtime_observation_lifetime
        .install_hook
        .lock()
        .unwrap() = Some(Arc::new(move |phase| {
        if phase != "capture" {
            return;
        }
        let mut inner = state.lock().unwrap();
        if inner
            .session_mut(&session)
            .unwrap()
            .engram
            .source_root_bind_owner
            .take()
            .is_some()
        {
            saw_owner.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }));
    f.turn.end();
    settle_close(&f.turn);
    *f.turn
        .state
        .runtime_observation_lifetime
        .install_hook
        .lock()
        .unwrap() = None;
    assert!(
        released.load(std::sync::atomic::Ordering::SeqCst),
        "the proof read raced the naming owner"
    );
    assert_not_refreshed(&f, &turn_ref);
}
