// Exercises caller-visible process evidence on the existing naming owner,
// including an applied remote event whose reply is lost and replayed unchanged.
use super::*;

struct NamingFailureObservation {
    inner: Arc<ScriptedEngramControlTransport>,
    process: Option<EngramControlProcessEvidence>,
}

impl EngramControlTransport for NamingFailureObservation {
    fn request(&self, connection: &EngramConnectionConfig, request: &EngramControlRequest,
        timeout: Duration) -> Result<Value, EngramTransportError> {
        self.inner.request(connection, request, timeout).map_err(|error| {
            if matches!(request, EngramControlRequest::NamedRootBind { .. })
                && let Some(process) = &self.process
            {
                error.with_control_process(process.clone())
            } else {
                error
            }
        })
    }

    fn read_observation_policy(&self, connection: &EngramConnectionConfig,
        store: &EngramAuthorityStoreKey, timeout: Duration, host_workdir: &FsPath)
        -> Result<EngramObservationPolicyBasis, EngramTransportError> {
        self.inner.read_observation_policy(connection, store, timeout, host_workdir)
    }

    fn read_work_binding(&self, connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>, timeout: Duration)
        -> Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.inner.read_work_binding(connection, preference, timeout)
    }

    fn read_held_claims(&self, connection: &EngramConnectionConfig, timeout: Duration)
        -> Result<EngramHeldClaims, EngramTransportError> {
        self.inner.read_held_claims(connection, timeout)
    }

    fn shutdown_session(&self, session: &str) { self.inner.shutdown_session(session); }
}

#[test]
fn named_root_failure_diagnostics_preserve_observed_child_and_pending_identity() {
    for clear in [false, true] {
        naming_failure_diagnostics(clear, Some(EngramControlProcessEvidence {
            pid: 24681,
            exit_state: "exited: exit code: 23".to_owned(),
        }));
    }
}

#[test]
fn named_root_failure_diagnostics_report_unavailable_without_inventing_a_child() {
    for clear in [false, true] {
        naming_failure_diagnostics(clear, None);
    }
}

fn naming_failure_diagnostics(clear: bool, process: Option<EngramControlProcessEvidence>) {
    let label = format!("failure-process-{clear}-{}", process.is_some());
    let claimed = EOCheckedTurn::idle(&label, true);
    let worktree = naming_worktree(&claimed.root);
    if clear {
        request_naming(&claimed, &label, Some(&worktree), vec![naming_claim(&label)]).unwrap();
    }
    let confirmed = claimed.state.inner.lock().unwrap()
        .engram_named_root_journal.first().and_then(|journal| journal.confirmed.clone());
    claimed.state.install_control_test_transport(Arc::new(NamingFailureObservation {
        inner: claimed.transport.clone(), process: process.clone(),
    }));
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap().lose_next_reply = true;
    let path = (!clear).then_some(worktree.as_path());
    let error = request_naming(&claimed, &label, path, vec![naming_claim(&label)]).unwrap_err();
    let response = error.into_response();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let bytes = runtime.block_on(axum::body::to_bytes(response.into_body(), 16 * 1024)).unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body.as_object().unwrap().len(), 1, "ordinary error response shape");
    let message = body["error"].as_str().unwrap();
    assert!(message.contains("control_unavailable"), "{message}");
    assert!(message.contains("fixture lost reply after durable event"), "{message}");
    match process {
        Some(process) => {
            assert!(message.contains(&format!("identity=pid {}", process.pid)), "{message}");
            assert!(message.contains(&format!("exit={}", process.exit_state)), "{message}");
        }
        None => assert!(message.contains("identity=unavailable; exit=unavailable"), "{message}"),
    }
    assert!(!message.contains(&worktree.to_string_lossy().to_string()));
    assert_eq!(claimed.state.engram_root_capture(&claimed.session_id), EngramRootCapture::Unconfirmed);
    let pending = {
        let inner = claimed.state.inner.lock().unwrap();
        let journal = &inner.engram_named_root_journal[0];
        assert_eq!(journal.confirmed, confirmed, "failure cannot acknowledge the new event");
        assert_eq!(inner.engram_work_source_roots.len(), usize::from(clear));
        journal.pending.clone().unwrap()
    };
    assert_eq!(pending.kind, if clear { EngramNamedRootKind::Ended } else { EngramNamedRootKind::Bound });
    assert_eq!(pending.end_reason, clear.then_some(EngramNamedRootEndReason::ExplicitClear));
    let original = serde_json::to_value(pending.request().unwrap()).unwrap();
    let sends = || claimed.transport.requests().into_iter()
        .filter(|request| request.request["operation"] == "named_root_bind")
        .map(|request| request.request).collect::<Vec<_>>();
    assert_eq!(sends().last(), Some(&original));
    assert_eq!(sends().len(), 1 + usize::from(clear), "no implicit retry");
    request_naming(&claimed, &label, path, vec![naming_claim(&label)]).unwrap();
    let sent = sends();
    assert_eq!(&sent[sent.len()-2..], &[original.clone(), original]);
    let inner = claimed.state.inner.lock().unwrap();
    assert!(inner.engram_named_root_journal[0].pending.is_none());
    assert_eq!(inner.engram_work_source_roots.len(), usize::from(!clear));
}
