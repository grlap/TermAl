//! Every binding writer respects the acknowledged set, whose older members
//! must already exist in the durable session row when the name is accepted.
use super::*;

fn durable_has_job(fixture: &TwoClaims, turn: &str) -> bool {
    let db = open_sqlite_state_read_connection(&fixture.turn.state.persistence_path).unwrap();
    let raw: String = db
        .query_row(
            "SELECT value_json FROM sessions WHERE id = ?1",
            [&fixture.turn.session_id],
            |row| row.get(0),
        )
        .unwrap();
    let saved: PersistedSessionRecord = serde_json::from_str(&raw).unwrap();
    saved
        .engram_unadmitted_closing_jobs
        .iter()
        .any(|job| job.trace.host_turn_ref == turn)
}

#[test]
fn canonical_focus_b_waits_for_an_older_acknowledged_a_retry() {
    let label = "snapshot-canonical-older-retry";
    let fixture = TwoClaims::new(label);
    super::super::super::super::canonical_focus::install_canonical_focus_reader(&fixture.turn);
    let older = older_turn_in_retry(&fixture);
    fixture
        .turn
        .begin("Y names B while X still awaits its retry.");
    fixture.check_first("Y-before-canonical-B");
    let mut held = naming_claim(label);
    assert_eq!(held.items[0].control_binding.as_ref(), Some(&fixture.first));
    held.items[0].focused = false;
    held.items.push(EngramHeldClaim {
        work_id: fixture.second.work_id.clone(),
        short_ref: "w-other-named-claim".into(),
        claim_id: fixture.second.claim_id.clone(),
        claim_fence: fixture.second.claim_fence,
        focused: true,
        control_binding: Some(fixture.second.clone()),
    });
    // Canonical focus changes independently of the producer's acceptance of
    // A's historical report, exactly as in the single-predecessor fixture.
    fixture.turn.transport.replace_held_claims([Ok(held)]);
    fixture.name_other();
    fixture.turn.end();
    let newer = fixture.turn.record(|record| {
        record
            .engram
            .unadmitted_closing_jobs
            .last()
            .unwrap()
            .clone()
    });
    queue_b(&fixture);
    fixture.release_worker();
    assert_report(&fixture, &newer, false);
    set_retry_due(
        &fixture,
        &older.trace.host_turn_ref,
        unadmitted_observation_timestamp(),
    );
    let guard = phase_sync::PollGuard::new();
    let (report, terminal) = loop {
        let result = fixture.turn.record(|record| {
            (
                record
                    .engram
                    .unadmitted_reports
                    .iter()
                    .find(|report| report.host_turn_ref == older.trace.host_turn_ref)
                    .cloned(),
                record
                    .engram
                    .unadmitted_closing_jobs
                    .iter()
                    .find(|job| job.trace.host_turn_ref == older.trace.host_turn_ref)
                    .and_then(|job| job.terminal_failure.clone()),
            )
        });
        if result.0.is_some() || result.1.is_some() {
            break result;
        }
        guard.wait("X reaches its actual capture outcome after Y's prospective focus refresh");
    };
    assert!(
        report.is_some(),
        "canonical B focus must not invalidate X's A report: {terminal:?}"
    );
    assert_report(&fixture, &older, false);
    let successor = loop {
        if let Some(trace) = fixture
            .turn
            .record(|record| record.engram.unadmitted_active_turn.clone())
        {
            break trace;
        }
        guard.wait("B opens after every acknowledged A capture");
    };
    assert_eq!(
        successor.opening_root.as_ref().unwrap().binding,
        fixture.second
    );
    fixture.wait_for_recorded_while_active(&older, &successor);
    fixture.check_in("B-after-canonical-older-retry", &fixture.second_root);
    fixture
        .turn
        .transport
        .enable_named_roots(&fixture.turn.session_id, &fixture.second);
    assert_eq!(fixture.turn.finish()["binding"], json!(fixture.second));
}

#[test]
fn an_unsaved_older_a_job_does_not_enter_the_acknowledged_snapshot() {
    let fixture = TwoClaims::new("snapshot-durable-cut");
    fixture
        .turn
        .begin("X closes after the naming authority image is acknowledged.");
    fixture.check_first("unsaved-X-check");
    fixture
        .turn
        .record_mut(|record| record.engram.unadmitted_worker_running = true);
    let naming_state = fixture.turn.state.clone();
    let naming_session = fixture.turn.session_id.clone();
    let naming_request = EngramSourceRootRequest {
        work: fixture.second.work_id.clone(),
        path: Some(Some(fixture.second_root.to_string_lossy().into_owned())),
    };
    let original_persist = fixture.turn.state.persist_tx.clone();
    let (delayed_persist, delayed_requests) = mpsc::channel::<PersistRequest>();
    let fixture = Arc::new(Mutex::new(fixture));
    let during = Arc::downgrade(&fixture);
    let crossed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let crossed_hook = crossed.clone();
    naming_state
        .inner
        .lock()
        .unwrap()
        .test_engram_authority_ack_boundary = Some(Arc::new(move |boundary| {
        if boundary != "before_publication" {
            return;
        }
        let fixture = during.upgrade().unwrap();
        let mut fixture = fixture.lock().unwrap();
        let requested_root_is_installed = {
            let inner = fixture.turn.state.inner.lock().unwrap();
            inner.engram_work_source_roots.iter().any(|root| {
                root.work_id == fixture.second.work_id && root.claim_id == fixture.second.claim_id
            })
        };
        if !requested_root_is_installed
            || crossed_hook.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        // This existing boundary follows the real authority fence's
        // exact readback. Only the optional manual-fixture cleanup below
        // publication is suppressed; no required acknowledgement is faked.
        TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(false));
        // The naming caller keeps its original sender. Runtime closes and
        // openings use a clone whose live receiver delays session writes.
        fixture.turn.state.persist_tx = delayed_persist.clone();
        fixture.turn.end();
        fixture
            .turn
            .begin("Y is the actual predecessor at acknowledgement.");
    }));
    struct RestoreManualWriter(bool);
    impl Drop for RestoreManualWriter {
        fn drop(&mut self) {
            TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(self.0));
        }
    }
    let manual_writer =
        RestoreManualWriter(TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.get()));
    let named = naming_state.name_engram_source_root(&naming_session, naming_request);
    naming_state
        .inner
        .lock()
        .unwrap()
        .test_engram_authority_ack_boundary = None;
    drop(manual_writer);
    let mut fixture = Arc::try_unwrap(fixture).ok().unwrap().into_inner().unwrap();
    let named = named.expect("required naming authority acknowledgement completed");
    assert_eq!(
        named.root.as_deref(),
        fs::canonicalize(&fixture.second_root).unwrap().to_str()
    );
    assert!(crossed.load(std::sync::atomic::Ordering::SeqCst));
    let older = fixture.turn.record(|record| {
        record
            .engram
            .unadmitted_closing_jobs
            .last()
            .unwrap()
            .clone()
    });
    assert_eq!(older.scope.binding.as_ref(), Some(&fixture.first));
    assert_eq!(older.trace.opening_scope.as_ref(), Some(&older.scope));
    assert!(
        !durable_has_job(&fixture, &older.trace.host_turn_ref),
        "X closed after the authority fence and is absent at the selection cut"
    );
    fixture.check_first("actual-naming-Y-check");
    let naming_turn = fixture.turn.record(|record| {
        record
            .engram
            .unadmitted_active_turn
            .as_ref()
            .unwrap()
            .host_turn_ref
            .clone()
    });
    assert!(
        !durable_has_job(&fixture, &older.trace.host_turn_ref),
        "X stayed unsaved across the acknowledgment"
    );
    assert!(
        delayed_requests
            .try_iter()
            .any(|request| matches!(request, PersistRequest::Delta)),
        "real session persistence was queued while the receiver was held"
    );
    let selection = fixture
        .turn
        .record(|record| record.engram.pending_naming_selection.clone().unwrap());
    assert_eq!(
        selection.named_turn_ref.as_deref(),
        Some(naming_turn.as_str())
    );
    let enrolled = selection
        .predecessors
        .iter()
        .find(|member| member.turn_ref == older.trace.host_turn_ref);
    assert!(
        enrolled.is_none(),
        "an older job absent from the durable ACK image cannot join S; adoptable={:?}",
        enrolled.map(|member| member.adoptable)
    );

    // Saving the queued job later cannot grant it the earlier cut's adoption
    // authority. Its normal refusal retains facts and a readable diagnostic.
    fixture.turn.state.persist_tx = original_persist;
    drop(delayed_requests);
    fixture.turn.end();
    let newer = fixture.turn.record(|record| {
        record
            .engram
            .unadmitted_closing_jobs
            .last()
            .unwrap()
            .clone()
    });
    assert!(durable_has_job(&fixture, &older.trace.host_turn_ref));
    let token = fixture
        .turn
        .record(|record| record.runtime.runtime_token().unwrap());
    fixture
        .turn
        .state
        .open_unmediated_claude_turn(
            &fixture.turn.session_id,
            &token,
            ClaudeUnownedCause::NoPrompt,
        )
        .unwrap()
        .unwrap();
    let (retained, successor) = fixture.turn.record(|record| {
        (
            record
                .engram
                .unadmitted_closing_jobs
                .iter()
                .find(|job| job.trace.host_turn_ref == older.trace.host_turn_ref)
                .unwrap()
                .clone(),
            record.engram.unadmitted_active_turn.clone().unwrap(),
        )
    });
    assert_eq!(retained, older, "later persistence cannot enroll or mark X");
    assert_eq!(
        successor.opening_root.as_ref().unwrap().binding,
        fixture.second
    );
    let refused = fixture
        .turn
        .state
        .capture_runtime_observation_report(&fixture.turn.session_id, &retained);
    assert_eq!(
        refused.as_ref().err().unwrap().code.as_deref(),
        Some("runtime_observation_scope_changed")
    );
    fixture
        .turn
        .state
        .install_runtime_closing_result(&fixture.turn.session_id, &retained, refused)
        .unwrap();
    let (failed, fabricated, diagnostic) = fixture.turn.record(|record| {
        (
            record
                .engram
                .unadmitted_closing_jobs
                .iter()
                .find(|job| job.trace.host_turn_ref == older.trace.host_turn_ref)
                .unwrap()
                .clone(),
            record
                .engram
                .unadmitted_reports
                .iter()
                .any(|report| report.host_turn_ref == older.trace.host_turn_ref),
            record.session.messages.iter().any(|message| {
                matches!(message, Message::Text { text, .. }
            if text.contains(&older.trace.host_turn_ref) && text.contains("not reported"))
            }),
        )
    });
    assert!(failed.terminal_failure.is_some());
    assert_eq!(failed.scope, older.scope);
    assert_eq!(failed.trace.commands, older.trace.commands);
    assert!(!fabricated);
    assert!(
        diagnostic,
        "X's own terminal no-report diagnostic stays readable"
    );
    fixture.release_worker();
    assert_report(&fixture, &newer, true);
    fixture.wait_for_recorded_while_active(&newer, &successor);
    fixture.check_in("B-after-unsaved-X", &fixture.second_root);
    fixture
        .turn
        .transport
        .enable_named_roots(&fixture.turn.session_id, &fixture.second);
    assert_eq!(fixture.turn.finish()["binding"], json!(fixture.second));
}

#[test]
fn queued_retry_progress_keeps_an_older_durable_a_scope_in_the_snapshot() {
    let fixture = TwoClaims::new("snapshot-durable-scope-queued-progress");
    let older = older_turn_in_retry(&fixture);
    fixture
        .turn
        .begin("Y names B while X's next failed attempt is queued.");
    fixture.check_first("Y-before-queued-retry-progress");
    let (delayed_persist, delayed_requests) = mpsc::channel::<PersistRequest>();
    let mut during = fixture.turn.state.clone();
    during.persist_tx = delayed_persist;
    let session = fixture.turn.session_id.clone();
    let second = fixture.second.clone();
    let expected = older.clone();
    let transport = fixture.turn.transport.clone();
    let crossed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let crossed_hook = crossed.clone();
    fixture
        .turn
        .state
        .inner
        .lock()
        .unwrap()
        .test_engram_authority_ack_boundary = Some(Arc::new(move |boundary| {
        if boundary != "before_publication" {
            return;
        }
        let requested_root_is_installed = during
            .inner
            .lock()
            .unwrap()
            .engram_work_source_roots
            .iter()
            .any(|root| root.work_id == second.work_id && root.claim_id == second.claim_id);
        if !requested_root_is_installed
            || crossed_hook.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(false));
        transport
            .observation_policy_errors
            .lock()
            .unwrap()
            .push_back(EngramTransportError::transport(
                "another temporary older-X policy outage",
            ));
        let failed = during.capture_runtime_observation_report(&session, &expected);
        assert!(failed.as_ref().is_err_and(|error| {
            error
                .message
                .contains("another temporary older-X policy outage")
        }));
        during
            .install_runtime_closing_result(&session, &expected, failed)
            .unwrap();
        let token = {
            let inner = during.inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&session).unwrap()]
                .runtime
                .runtime_token()
                .unwrap()
        };
        during
            .finish_turn_ok_if_runtime_matches(&session, &token)
            .unwrap();
    }));
    struct RestoreCutHook(AppState, bool);
    impl Drop for RestoreCutHook {
        fn drop(&mut self) {
            self.0
                .inner
                .lock()
                .unwrap()
                .test_engram_authority_ack_boundary = None;
            TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(self.1));
        }
    }
    let hook = RestoreCutHook(
        fixture.turn.state.clone(),
        TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.get()),
    );
    fixture.name_other();
    drop(hook);
    assert!(crossed.load(std::sync::atomic::Ordering::SeqCst));
    let saved = saved_job(&fixture, &older.trace.host_turn_ref);
    let (current, retained) = fixture.turn.record(|record| {
        let current = record
            .engram
            .unadmitted_closing_jobs
            .iter()
            .find(|job| job.trace.host_turn_ref == older.trace.host_turn_ref)
            .unwrap()
            .clone();
        let retained = record
            .engram
            .pending_naming_selection
            .as_ref()
            .unwrap()
            .predecessors
            .iter()
            .any(|member| {
                member.turn_ref == older.trace.host_turn_ref
                    && member.scope.as_ref() == Some(&older.scope)
                    && member.adoptable
            });
        (current, retained)
    });
    assert_eq!(saved.attempts, 1);
    assert_eq!(current.attempts, 2);
    assert_eq!(saved.scope, current.scope);
    assert!(
        delayed_requests
            .try_iter()
            .any(|request| matches!(request, PersistRequest::Delta))
    );
    assert!(
        retained,
        "queued retry progress cannot erase X's durable A custody"
    );
    let newer = fixture.turn.record(|record| {
        record
            .engram
            .unadmitted_closing_jobs
            .last()
            .unwrap()
            .clone()
    });
    assert!(!durable_has_job(&fixture, &newer.trace.host_turn_ref));
    fixture.turn.record(|record| {
        let selection = record.engram.pending_naming_selection.as_ref().unwrap();
        assert_eq!(
            selection.named_turn_ref.as_ref(),
            Some(&newer.trace.host_turn_ref)
        );
        assert!(
            selection
                .predecessors
                .iter()
                .any(|member| member.turn_ref == newer.trace.host_turn_ref
                    && member.adoptable
                    && member.scope.as_ref() == Some(&newer.scope)),
            "the actual naming turn keeps its own exact live custody after closing during ACK"
        );
    });
    // Park X again only after proving the queued attempt was not persisted at
    // the cut. Both real failed attempts retain their unchanged retry budget.
    set_retry_due(
        &fixture,
        &older.trace.host_turn_ref,
        (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
    );
    queue_b(&fixture);
    fixture.release_worker();
    assert_report(&fixture, &newer, false);
    fixture.turn.record(|record| {
        assert!(record.engram.unadmitted_active_turn.is_none());
        assert_eq!(record.engram.work_binding.as_ref(), Some(&fixture.first));
        assert_eq!(record.queued_prompts.len(), 1);
    });
    finish_b_after_older_capture(&fixture, &older);
}
