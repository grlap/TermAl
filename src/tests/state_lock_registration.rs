// Registration evidence at the real typed mutex and pending-response boundary.
// Barriers determine overlap; elapsed time is never a pass/fail threshold.
use super::*;

#[path = "state_lock_observer.rs"]
mod observer;

#[test]
fn review_observer_release_publication_gap_is_unknown() {
    let state = test_app_state();
    let held = state.inner.lock().unwrap();
    let (requested_tx, requested_rx) = mpsc::channel();
    let (collected_tx, collected_rx) = mpsc::channel();
    let waiter = {
        let inner = Arc::clone(&state.inner);
        let instance = state.server_instance_id.clone();
        std::thread::spawn(move || {
            let diagnostic = TestRunRegistrationDiagnostics::new(&instance);
            TEST_STATE_MUTEX_REQUESTED.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || {
                    let _ = requested_tx.send(());
                }));
            });
            drop(diagnostic.lock(&inner));
            let _ = collected_tx.send(diagnostic.state_lock_block());
        })
    };
    let requested = requested_rx.recv_timeout(phase_sync::DEADLOCK_GUARD);
    let observed = std::rc::Rc::new(std::cell::RefCell::new(None));
    let captured = std::rc::Rc::clone(&observed);
    TEST_STATE_MUTEX_UNLOCKED.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            // The primary mutex is unlocked, but this predecessor has not
            // appended its release. Let the real waiter finish and collect.
            *captured.borrow_mut() = Some(collected_rx.recv_timeout(phase_sync::DEADLOCK_GUARD));
        }));
    });
    drop(held);
    let joined = waiter.join();
    requested.expect("waiter requested before the predecessor unlocked");
    joined.unwrap();
    let block = observed.borrow_mut().take().unwrap().unwrap();
    assert!(
        block.partial && block.unavailable,
        "an outstanding predecessor release must prevent complete-looking evidence: {block:?}"
    );
}

fn contended_registration_diagnostics() -> (TestRunRegistrationDiagnostics, u32, u32) {
    let state = test_app_state();
    let holder_line = line!() + 1;
    let held = state.inner.lock().unwrap();
    let (requested_tx, requested_rx) = mpsc::channel();
    let waiter = {
        let inner = Arc::clone(&state.inner);
        let instance = state.server_instance_id.clone();
        std::thread::spawn(move || {
            let mut diagnostic = TestRunRegistrationDiagnostics::new(&instance);
            diagnostic.enter("resolve_project");
            TEST_STATE_MUTEX_REQUESTED.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || requested_tx.send(()).unwrap()));
            });
            let waiter_line = line!() + 1;
            drop(diagnostic.lock(&inner));
            (diagnostic, waiter_line)
        })
    };
    // Even a failed barrier releases the production guard and joins its waiter.
    let requested = requested_rx.recv_timeout(phase_sync::DEADLOCK_GUARD);
    drop(held);
    let (diagnostic, waiter_line) = waiter.join().unwrap();
    requested.expect("waiter must request while the direct holder owns StateInner");
    (diagnostic, holder_line, waiter_line)
}

async fn pending_response(diagnostic: &TestRunRegistrationDiagnostics) -> Value {
    let identity = json!({
        "runId": "test-state-lock-evidence", "ownerSessionId": "session-owner",
        "requestDigest": "a".repeat(64)
    });
    let mut error = ApiError::enqueued_durability_deadline("fixture pending", identity.clone())
        .at_durability_boundary(DurabilityPendingOperation::TestRunRegistration);
    diagnostic.report("test-state-lock-evidence", &mut error);
    let response = error.into_response();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let bytes = to_bytes(response.into_body(), 65_536).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["identity"], identity);
    assert_eq!(body["outcome"], "pending");
    assert_eq!(body["retry"], json!({"mode": "same_request"}));
    body
}

#[tokio::test]
async fn registration_pending_retains_the_overlapping_direct_holder_and_helper_wait_site() {
    let (diagnostic, holder_line, waiter_line) = contended_registration_diagnostics();
    let body = pending_response(&diagnostic).await;
    let block = body.get("stateLock").expect(
        "registration pending response must retain bounded holder evidence beyond console text",
    );
    assert_eq!(block["contract"], "termal-state-lock-observation");
    assert_eq!(block["version"], 1);
    assert!(serde_json::to_vec(block).unwrap().len() <= 2048);
    let waits = block["waits"].as_array().expect("wait observations");
    let wait = waits
        .iter()
        .find(|row| row["site"]["line"] == waiter_line)
        .expect("registration helper must propagate its caller site");
    let holders = block["holders"].as_array().expect("holder observations");
    let holder = holders
        .iter()
        .find(|row| row["site"]["line"] == holder_line)
        .expect("the direct StateInner acquisition must be observed");
    assert_eq!(holder["state"], "released");
    assert_ne!(holder["holdId"], wait["holdId"]);
    assert!(holder["acquiredMicros"].as_u64().unwrap() < wait["acquiredMicros"].as_u64().unwrap());
    assert!(holder["releasedMicros"].as_u64().unwrap() > wait["requestedMicros"].as_u64().unwrap());
}

#[tokio::test]
async fn unavailable_registration_metadata_is_unknown_without_changing_pending_identity() {
    let (diagnostic, _, _) = contended_registration_diagnostics();
    let held = diagnostic.lock_timings.lock().unwrap();
    let body = pending_response(&diagnostic).await;
    assert_eq!(body["stateLock"]["unavailable"], true);
    assert_eq!(body["stateLock"]["partial"], true);
    drop(held);
    let timings = Arc::clone(&diagnostic.lock_timings);
    assert!(
        std::thread::spawn(move || {
            let _held = timings.lock().unwrap();
            panic!("fixture registration diagnostic poison");
        })
        .join()
        .is_err()
    );
    assert_eq!(
        pending_response(&diagnostic).await["stateLock"]["unavailable"],
        true
    );
}

#[test]
fn busy_registration_metadata_cannot_strand_guard_release() {
    let state = test_app_state();
    let diagnostic = TestRunRegistrationDiagnostics::new(&state.server_instance_id);
    let timings = diagnostic.lock_timings.lock().unwrap();
    drop(diagnostic.lock(&state.inner));
    assert!(state.inner.try_lock_now().is_some());
    assert_eq!(diagnostic.lock_observation_loss.load(Ordering::Relaxed), 1);
    drop(timings);
}

#[test]
fn abandoned_response_keeps_same_bounded_evidence_in_the_existing_exact_trace() {
    let (mut diagnostic, _, _) = contended_registration_diagnostics();
    let traces = TestRunPhaseTraces::default();
    diagnostic.trace = Some(traces.begin(
        "registration",
        Some("test-abandoned-evidence"),
        "evidence-operation",
        diagnostic.started,
    ));
    let mut error = ApiError::enqueued_durability_deadline(
        "pending",
        json!({"runId":"test-abandoned-evidence"}),
    )
    .at_durability_boundary(DurabilityPendingOperation::TestRunRegistration);
    diagnostic.report("test-abandoned-evidence", &mut error);
    let expected = serde_json::to_value(
        error
            .durability_deadline
            .as_ref()
            .unwrap()
            .state_lock
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    // No caller reads the response. Evidence retention precedes serialization
    // and does not mean the caller received it or that a fence did not apply.
    drop(error.into_response());
    drop(diagnostic);
    let selected = traces.selected(Some("test-abandoned-evidence"), Some("evidence-operation"));
    assert_eq!(selected.len(), 1);
    assert!(selected[0].completed);
    assert_eq!(
        serde_json::to_value(selected[0].state_lock.as_ref().unwrap()).unwrap(),
        expected
    );
    assert!(serde_json::to_vec(&expected).unwrap().len() <= 2048);
    assert!(traces.selected(Some("test-unrelated"), None).is_empty());
}

#[tokio::test]
async fn advisory_evidence_is_not_attached_to_other_pending_operations_or_plain_refusals() {
    let (diagnostic, _, _) = contended_registration_diagnostics();
    for operation in [
        DurabilityPendingOperation::AcceptanceEvaluationSubmission,
        DurabilityPendingOperation::NamedRootNaming,
        DurabilityPendingOperation::NamedRootRelease,
    ] {
        let identity = json!({"original":"identity"});
        let mut error = ApiError::enqueued_durability_deadline("pending", identity.clone())
            .at_durability_boundary(operation);
        diagnostic.report("test-other-operation", &mut error);
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 65_536).await.unwrap()).unwrap();
        assert_eq!(body["identity"], identity);
        assert!(body.get("stateLock").is_none());
    }
    let mut refusal = ApiError::conflict("immutable request changed");
    diagnostic.report("test-refused", &mut refusal);
    let response = refusal.into_response();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65_536).await.unwrap()).unwrap();
    assert!(body.get("stateLock").is_none());
}
