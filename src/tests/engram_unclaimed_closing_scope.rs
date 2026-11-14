//! Delayed closing capture keeps the closed turn's work ownership.
//! Uses ordinary provider turns and the existing worker installation boundary.
use super::*;

struct ClosingProbe {
    durable: mpsc::Receiver<Result<bool, String>>,
    failed: mpsc::Receiver<EngramRuntimeClosingJob>,
    settled: mpsc::Receiver<(
        Option<EngramRuntimeClosingJob>,
        Vec<EngramRuntimeObservationReport>,
    )>,
    release: mpsc::Sender<()>,
}

fn probe_closing(turn: &EOCheckedTurn, turn_ref: &str) -> ClosingProbe {
    let (durable_tx, durable) = mpsc::channel();
    let (failed_tx, failed) = mpsc::channel();
    let (settled_tx, settled) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let first_failure = std::sync::atomic::AtomicBool::new(false);
    let initial_durability = std::sync::atomic::AtomicBool::new(false);
    let inner = turn.state.inner.clone();
    let persistence_path = turn.state.persistence_path.clone();
    let session = turn.session_id.clone();
    let turn_ref = turn_ref.to_owned();
    *turn
        .state
        .runtime_observation_lifetime
        .install_hook
        .lock()
        .unwrap() = Some(Arc::new(move |phase| {
        if phase != "closing_installed" && phase != "capture" {
            return;
        }
        let (job, reports) = {
            let inner = inner.lock().unwrap();
            let state = &inner.sessions[inner.find_session_index(&session).unwrap()].engram;
            (
                state
                    .unadmitted_closing_jobs
                    .iter()
                    .find(|job| job.trace.host_turn_ref == turn_ref)
                    .cloned(),
                state
                    .unadmitted_reports
                    .iter()
                    .filter(|report| report.host_turn_ref == turn_ref)
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        };
        if phase == "capture" {
            if let Some(job) = job
                && !initial_durability.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                let _ = durable_tx.send(
                    runtime_observation_session_entry_is_durable(
                        &persistence_path,
                        &session,
                        "engramUnadmittedClosingJobs",
                        &serde_json::to_value(job).unwrap(),
                    )
                    .map_err(|error| error.message),
                );
            }
            return;
        }
        if let Some(failed) = job.as_ref().filter(|job| job.last_error.is_some())
            && !first_failure.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let _ = failed_tx.send(failed.clone());
            // Dropping the sender also releases assertion-failure teardown.
            let _ = release_rx
                .lock()
                .unwrap()
                .recv_timeout(phase_sync::DEADLOCK_GUARD);
            return;
        }
        if job
            .as_ref()
            .is_none_or(|job| job.terminal_failure.is_some())
        {
            let _ = settled_tx.send((job, reports));
        }
    }));
    ClosingProbe {
        durable,
        failed,
        settled,
        release,
    }
}

fn observed_check(turn: &EOCheckedTurn, key: &str) {
    turn.run(key, "cargo test", EngramCommandExit::Code(0), || {});
    turn.record(|record| {
        let trace = record.engram.unadmitted_active_turn.as_ref().unwrap();
        let command = trace
            .commands
            .iter()
            .find(|command| command.tool_key == key)
            .unwrap();
        assert_eq!(
            command.check.observed_result,
            EngramObservedCheckResult::Passed
        );
    });
}

fn late_claim_cannot_take_a_closed_turn(successor_started: bool) {
    let label = if successor_started {
        "unclaimed-successor"
    } else {
        "unclaimed-later-claim"
    };
    let turn = EOCheckedTurn::start(label, false);
    let original_ref = observed_turn_ref(&turn);
    observed_check(&turn, "original-check");
    let probe = probe_closing(&turn, &original_ref);
    turn.transport
        .work_bindings
        .lock()
        .unwrap()
        .push_back(Err(EngramTransportError::transport(
            "transient original closing binding read",
        )));
    turn.end();
    assert!(
        phase_sync::receive(&probe.durable, "the initial closing image is durable")
            .expect("read the original closing image")
    );
    let failed = phase_sync::receive(&probe.failed, "the unclaimed capture failed and backed off");
    assert!(failed.scope.binding.is_none());
    assert!(failed.trace.opening_root.is_none());
    assert_eq!(failed.attempts, 1);
    assert_eq!(
        failed.last_error.as_deref(),
        Some("transient original closing binding read")
    );
    assert!(
        chrono::DateTime::parse_from_rfc3339(&failed.retry_at).unwrap()
            > chrono::DateTime::parse_from_rfc3339(&failed.trace.adopted_at).unwrap()
    );
    assert_eq!(failed.trace.commands.len(), 1);
    assert_eq!(failed.trace.commands[0].tool_key, "original-check");

    if successor_started {
        turn.begin("A successor starts before taking a new claim.");
        turn.record(|record| assert!(record.active_turn_generation > failed.trace.generation));
    }
    // This is the fixture's external work-service claim transition, after N
    // closed. It is not a root name or a rewrite of N's immutable job/trace.
    let later = test_control_work_binding(&format!("eo-check-{label}"), 1);
    turn.transport.set_current_work_binding(Some(later.clone()));
    turn.transport.replace_held_claims([Ok(EngramHeldClaims {
        items: vec![EngramHeldClaim {
            work_id: later.work_id.clone(),
            short_ref: "w-later-work".into(),
            claim_id: later.claim_id.clone(),
            claim_fence: later.claim_fence,
            focused: true,
            control_binding: Some(later.clone()),
        }],
        omitted: 0,
    })]);
    turn.record_mut(|record| record.engram.work_binding = Some(later.clone()));
    turn.acknowledge_unnamed_root();
    let retry_job = {
        let mut inner = turn.state.inner.lock().unwrap();
        let record = inner.session_mut(&turn.session_id).unwrap();
        let job = record
            .engram
            .unadmitted_closing_jobs
            .iter_mut()
            .find(|job| job.trace.host_turn_ref == original_ref)
            .unwrap();
        assert_eq!(
            &*job, &failed,
            "the failed closed-turn image was not rescoped by the later claim"
        );
        // The retry deadline has been observed above. Advance eligibility at
        // this controlled worker boundary instead of waiting for wall time.
        job.retry_at = "1970-01-01T00:00:00.000Z".into();
        let retry_job = job.clone();
        turn.state
            .commit_session_locked(&mut inner, &turn.session_id)
            .expect("persist the changed retry image before releasing its worker");
        retry_job
    };
    assert!(
        runtime_observation_session_entry_is_durable(
            &turn.state.persistence_path,
            &turn.session_id,
            "engramUnadmittedClosingJobs",
            &serde_json::to_value(&retry_job).unwrap(),
        )
        .expect("read back the changed retry image")
    );
    probe.release.send(()).unwrap();
    let (terminal, reports) =
        phase_sync::receive(&probe.settled, "the original closing job settles");
    assert!(
        reports.is_empty(),
        "an unclaimed closed turn must not become a report for a later claim: {:?}",
        reports
            .iter()
            .flat_map(|report| &report.envelopes)
            .map(|envelope| envelope.request.get("binding"))
            .collect::<Vec<_>>()
    );
    assert!(
        terminal
            .as_ref()
            .is_none_or(|job| job.terminal_failure.is_some())
    );
    assert!(
        ordinary_turn_requests(&turn, &original_ref).is_empty(),
        "the later work must receive none of the original turn's commands or check results"
    );
    *turn
        .state
        .runtime_observation_lifetime
        .install_hook
        .lock()
        .unwrap() = None;

    if !successor_started {
        turn.begin("The later claim can own its own next turn.");
    }
    observed_check(&turn, "later-check");
    let later_report = turn.finish();
    assert_eq!(later_report["binding"], json!(later));
    let checks = later_report["occurrence"]["observed_checks"]
        .as_array()
        .unwrap();
    assert_eq!(
        checks.len(),
        1,
        "only the later turn's own check reaches its work item"
    );
    assert_ne!(later_report["occurrence"]["host_turn_ref"], original_ref);
    assert_ordinary_observation_has_no_native_credit(&turn, &later_report);
}

#[test]
fn unclaimed_closing_retry_never_adopts_a_successor_turns_claim() {
    late_claim_cannot_take_a_closed_turn(true);
}

#[test]
fn unclaimed_closing_retry_never_adopts_a_claim_taken_after_its_end_without_a_successor() {
    late_claim_cannot_take_a_closed_turn(false);
}

#[test]
fn unclaimed_closing_without_a_later_claim_still_settles_without_a_work_report() {
    let turn = EOCheckedTurn::start("unclaimed-closing-positive", false);
    let original_ref = observed_turn_ref(&turn);
    observed_check(&turn, "ordinary-unclaimed-check");
    let probe = probe_closing(&turn, &original_ref);
    turn.end();
    assert!(
        phase_sync::receive(&probe.durable, "the initial closing image is durable")
            .expect("read the original closing image")
    );
    let (job, reports) = phase_sync::receive(&probe.settled, "ordinary no-claim closing succeeds");
    assert!(job.is_none());
    assert!(reports.is_empty());
    assert!(ordinary_turn_requests(&turn, &original_ref).is_empty());
    assert!(
        probe.failed.try_recv().is_err(),
        "the positive control needs no retry"
    );
    assert_ordinary_turn_has_no_native_requests(&turn);
    *turn
        .state
        .runtime_observation_lifetime
        .install_hook
        .lock()
        .unwrap() = None;
}
