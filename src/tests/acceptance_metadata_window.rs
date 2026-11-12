// Full committed-input evaluation requests through the real metadata owner.
// Only subprocess completion and tracker replies are scripted; executable
// custody, source parsing, revalidation and evaluator admission stay real.
use super::*;
use crate::tests::engram_source_owner::source_owner_reply;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

#[derive(Default)]
struct ProbeTrace {
    elapsed_ms: AtomicU64,
    versions: AtomicUsize,
    revalidations: AtomicUsize,
    final_sources: AtomicUsize,
    warnings: Mutex<Vec<Duration>>,
    failures: Mutex<Vec<EngramTransportErrorKind>>,
    tracker_phases: Mutex<Vec<String>>,
}

fn request_with_slow_version(
    slow_version: Duration,
) -> (
    Result<AcceptanceEvaluationRequestResponse, ApiError>,
    AppState,
    Arc<ProbeTrace>,
) {
    let (state, parent, _store, named_root) = named_root_parent();
    let head = commit_named_root(&named_root);
    acceptance_owner_reports(&state, &parent, &named_root, Vec::new(), Vec::new());
    let transport = install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
    let registration = register_committed_run(
        &state,
        &parent,
        &named_root,
        committed_results("passed", input_record(&head, "clean")),
    );
    host_observed(&state, &registration, &head, acknowledged("received"));
    let trace = Arc::new(ProbeTrace::default());
    let original_capture = state
        .inner
        .lock()
        .unwrap()
        .test_acceptance_source_owner_capture
        .clone()
        .unwrap();
    let original_final = state
        .inner
        .lock()
        .unwrap()
        .test_observation_source_owner
        .clone()
        .unwrap();
    let started = std::time::Instant::now();
    let deadline = started + acceptance_evaluation_request_tracker_budget();
    {
        let observed = trace.clone();
        state
            .inner
            .lock()
            .unwrap()
            .test_native_source_owner_metadata =
            Some(Arc::new(move |state, session, caller_deadline| {
                assert_eq!(
                    caller_deadline, deadline,
                    "the request deadline is never refreshed"
                );
                state.source_owner_metadata_snapshot_with(
                    session,
                    caller_deadline,
                    |_, _, args, until, _| {
                        if args != ["--version"] {
                            assert_eq!(until, deadline, "the longer source window is preserved");
                            return Ok(source_owner_reply(args));
                        }
                        let attempt = observed.versions.fetch_add(1, Ordering::SeqCst);
                        let latency = if attempt == 0 {
                            slow_version
                        } else {
                            Duration::from_secs(1)
                        };
                        // Convert the actual issued deadline to a local elapsed
                        // budget. The fixture advances only logical time, then
                        // charges that elapsed time to the full request clock.
                        let budget = until.saturating_duration_since(std::time::Instant::now());
                        let elapsed = std::cell::Cell::new(Duration::ZERO);
                        let result = wait_engram_cli_command(
                            || Ok((elapsed.get() >= latency).then(|| source_owner_reply(args))),
                            || elapsed.get(),
                            || elapsed.set(elapsed.get() + Duration::from_secs(1)),
                            budget,
                            ENGRAM_CALL_WARNING_AFTER,
                            "source owner version probe",
                            |spent| {
                                let mut warned = false;
                                warn_slow_engram_call(
                                    std::time::Instant::now() - spent,
                                    &mut warned,
                                    "source owner version probe",
                                );
                                assert!(warned, "the shared diagnostic was emitted");
                                observed.warnings.lock().unwrap().push(spent);
                            },
                        );
                        observed
                            .elapsed_ms
                            .fetch_add(elapsed.get().as_millis() as u64, Ordering::SeqCst);
                        result.map_err(|error| {
                            observed.failures.lock().unwrap().push(error.kind);
                            anyhow::anyhow!(error.message)
                        })
                    },
                )
            }));
        let observed = trace.clone();
        state
            .inner
            .lock()
            .unwrap()
            .test_acceptance_source_owner_capture =
            Some(Arc::new(move |binary, cwd, args, until, limit| {
                assert_eq!(
                    until, deadline,
                    "preflight source revalidation keeps its original window"
                );
                observed.revalidations.fetch_add(1, Ordering::SeqCst);
                original_capture(binary, cwd, args, until, limit)
            }));
        let observed = trace.clone();
        state.inner.lock().unwrap().test_observation_source_owner =
            Some(Arc::new(move |binary, cwd, args, until, limit| {
                if args[0] == "source" {
                    observed.final_sources.fetch_add(1, Ordering::SeqCst);
                }
                original_final(binary, cwd, args, until, limit)
            }));
    }
    let reader = independent_reader();
    let observed = trace.clone();
    let clock = trace.clone();
    let response = state.request_acceptance_evaluation_until(
        &parent,
        evaluation_request(Some(Agent::Codex)),
        move |connection, args, timeout| {
            let phase = if args.iter().any(|arg| arg == "held") {
                "held"
            } else if args.iter().any(|arg| arg == "--full") {
                "full"
            } else if args.first().map(String::as_str) == Some("control-policy") {
                "policy"
            } else if args.iter().any(|arg| arg == "inspect") {
                "core"
            } else {
                "show"
            };
            observed
                .tracker_phases
                .lock()
                .unwrap()
                .push(phase.to_owned());
            let reply = reader(connection, args, timeout);
            observed
                .elapsed_ms
                .fetch_add(ENGRAM_MIN_CALL_TIMEOUT.as_millis() as u64, Ordering::SeqCst);
            reply
        },
        deadline,
        move || started + Duration::from_millis(clock.elapsed_ms.load(Ordering::SeqCst)),
    );
    if let Ok(AcceptanceEvaluationRequestResponse::Spawned { delegation, .. }) = &response {
        assert_judges_the_admitted_commit(
            delegation
                .delegation
                .acceptance_evaluation
                .as_ref()
                .unwrap(),
            &registration,
            &named_root,
            &head,
            "slow version",
        );
    }
    assert_eq!(sighting_reads(&transport), 0, "no sighting fallback");
    (response, state, trace)
}

#[test]
fn a_slow_version_keeps_the_full_request_revalidation_source_and_evaluator_funded() {
    let (response, state, trace) = request_with_slow_version(Duration::from_secs(55));
    assert!(matches!(
        response.unwrap(),
        AcceptanceEvaluationRequestResponse::Spawned { .. }
    ));
    assert_eq!(
        trace.versions.load(Ordering::SeqCst),
        2,
        "bootstrap and preflight recheck, no resend"
    );
    assert_eq!(trace.revalidations.load(Ordering::SeqCst), 2);
    assert_eq!(trace.final_sources.load(Ordering::SeqCst), 1);
    assert_eq!(state.inner.lock().unwrap().delegations.len(), 1);
    assert_eq!(
        trace.warnings.lock().unwrap().as_slice(),
        [ENGRAM_CALL_WARNING_AFTER]
    );
    assert!(trace.failures.lock().unwrap().is_empty());
    assert_eq!(
        trace.tracker_phases.lock().unwrap().as_slice(),
        ["core", "show", "held", "full", "show", "core", "policy"]
    );
    assert!(
        Duration::from_millis(trace.elapsed_ms.load(Ordering::SeqCst))
            < acceptance_evaluation_request_tracker_budget()
    );
}

#[test]
fn a_version_past_its_own_window_returns_the_existing_refusal_without_an_evaluator() {
    let (response, state, trace) = request_with_slow_version(Duration::from_secs(70));
    let error = response
        .err()
        .expect("an expired version cannot establish owner provenance");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error.message.starts_with("committed_input_not_clean:"),
        "{}",
        error.message
    );
    assert_eq!(
        trace.failures.lock().unwrap().as_slice(),
        [EngramTransportErrorKind::Deadline]
    );
    assert_eq!(
        trace.warnings.lock().unwrap().as_slice(),
        [ENGRAM_CALL_WARNING_AFTER]
    );
    assert_eq!(
        trace.versions.load(Ordering::SeqCst),
        1,
        "no timeout resend"
    );
    assert_eq!(trace.revalidations.load(Ordering::SeqCst), 0);
    assert_eq!(trace.final_sources.load(Ordering::SeqCst), 0);
    assert!(state.inner.lock().unwrap().delegations.is_empty());
    assert!(
        Duration::from_millis(trace.elapsed_ms.load(Ordering::SeqCst))
            < acceptance_evaluation_request_tracker_budget(),
        "the process cannot consume the request's tail"
    );
}
