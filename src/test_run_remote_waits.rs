// Remote observations reuse the authenticated native relay and its supervisor.
// Origin facts outlive any destination wait. This module grants no verification
// credit and never reads a remote path, launches a test or creates a local run.

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoteTestRunIdentity {
    origin: TestRunNativeOrigin,
    run_id: String,
    request_identity: String,
    locator_identity: String,
    started_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoteTestRunFacts {
    identity: RemoteTestRunIdentity,
    completion: Option<native_test_completion::Completion>,
    /// Keep the first contradictory terminal as well as the original.
    conflict: Option<native_test_completion::Completion>,
    identity_conflict: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoteTestRunWaitLabel {
    host: FederationHostIdentity,
    project_id: String,
    creation_id: String,
    project_scope: Option<String>,
    identity: Option<RemoteTestRunIdentity>,
    facts: Option<RemoteTestRunFacts>,
    detail: String,
    // Restart requires current authenticated authority and durable read-back.
    #[serde(skip)]
    confirmed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoteTestRunReply {
    identity: Option<RemoteTestRunIdentity>,
    completion: Option<native_test_completion::Completion>,
    detail: String,
}

fn remote_test_run_reference(value: &str) -> Result<Option<(String, String)>, ApiError> {
    let Some(qualified) = value.strip_prefix("host:") else {
        return Ok(None); // Local identifiers retain their existing validation.
    };
    let Some((host, run)) = qualified.split_once("::") else {
        return Err(ApiError::bad_request("use host:<host UUID>::<native run id>"));
    };
    if Uuid::parse_str(host).ok().filter(|id| !id.is_nil()).map(|id| id.to_string()).as_deref() != Some(host)
        || run.is_empty() || run.len() > TEST_RUN_TEXT_MAX_BYTES
        || !run.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
    {
        return Err(ApiError::bad_request("invalid host-qualified test run reference"));
    }
    Ok(Some((host.to_owned(), run.to_owned())))
}

fn remote_test_run_scope(inner: &StateInner, project: &str) -> Option<String> {
    inner.find_project(project)?.engram.as_ref()?.authority_store_key.as_ref()
        .map(|key| key.project_id.clone())
        .filter(|scope| !scope.is_empty() && scope.len() <= 1024)
}

fn remote_test_run_facts_key(host: &FederationHostIdentity, run: &str) -> String {
    format!("{}:{}::{run}", host.host_id, host.host_generation)
}

fn remote_test_run_wait_label(
    inner: &StateInner, session: usize, project: &str, reference: &str, host: &str, _run: &str,
) -> Result<TestRunWaitRunLabel, ApiError> {
    let peer = federation_snapshot_locked(inner, host)?;
    if inner.federation.local.as_ref().is_some_and(|local| local.host_id == host) {
        return Err(ApiError::bad_request("use a bare run id for a local run"));
    }
    let remote = RemoteTestRunWaitLabel {
        host: peer.peer, project_id: project.to_owned(),
        creation_id: inner.sessions[session].federation_native_creation_id.clone(),
        project_scope: remote_test_run_scope(inner, project),
        identity: None, facts: None, confirmed: false,
        detail: "association_stale: awaiting authenticated native origin facts".to_owned(),
    };
    Ok(TestRunWaitRunLabel {
        remote: Some(remote), run_id: reference.to_owned(), run_dir: String::new(),
        preset: TestRunPreset::Focused, worktree: String::new(), owner_session_id: None,
        started_at: None, request_identity: None,
        locator_identity: None, registration_backed: false,
        registration_progress: TestRunWaitRegistrationProgress::default(),
    })
}

fn remote_test_run_wait_current(inner: &StateInner, wait: &TestRunWaitRecord, label: &RemoteTestRunWaitLabel) -> bool {
    inner.find_visible_session_index(&wait.session_id).is_some_and(|index| {
        let record = &inner.sessions[index];
        record.federation_native_creation_id == label.creation_id
            && record.session.project_id.as_deref() == Some(label.project_id.as_str())
            && label.project_scope.is_some()
            && remote_test_run_scope(inner, &label.project_id) == label.project_scope
            && federation_snapshot_locked(inner, &label.host.host_id)
                .is_ok_and(|peer| peer.peer == label.host)
    })
}

fn remote_test_run_wait_verdict(
    inner: &StateInner, wait: &TestRunWaitRecord, remote: &RemoteTestRunWaitLabel,
) -> Option<TestRunWaitVerdict> {
    if !remote.confirmed || !remote_test_run_wait_current(inner, wait, remote) { return None; }
    let facts = remote.facts.as_ref()?;
    if facts.identity_conflict || facts.conflict.is_some() {
        return Some(TestRunWaitVerdict::RemoteConflict);
    }
    let completion = facts.completion.as_ref().filter(|value| value.authoritative_terminal())?;
    Some(match completion.result.verdict {
        native_test_completion::Verdict::Pass => TestRunWaitVerdict::Pass,
        native_test_completion::Verdict::Fail | native_test_completion::Verdict::Interrupted => TestRunWaitVerdict::Fail,
        _ => return None,
    })
}

fn remote_test_run_wait_section(label: &TestRunWaitRunLabel, remote: &RemoteTestRunWaitLabel, verdict: Option<TestRunWaitVerdict>) -> String {
    let completion = remote.facts.as_ref().and_then(|facts| facts.completion.as_ref());
    let native = completion.map(|value| &value.result);
    format!("### Remote run `{}`\n- Verdict: {}\n- Origin host: `{}` (generation {})\n- Native runner: {}\n- Request identity: {}\n- Native terminal: {}\n- Native exit: {}\n- Native reason: {}\n- Finished: {}\n- Credit: uncredited (producer_unsupported: remote_verification_record_v1 destination admission is unavailable)\n- Observation: {}\n- Inspect original launcher evidence on the origin host; END mail remains the fallback.",
        label.run_id, test_run_wait_verdict_label(verdict), remote.host.host_id, remote.host.host_generation,
        remote.identity.as_ref().map_or("pending", |id| id.origin.native_session_id.as_str()),
        remote.identity.as_ref().map_or("pending", |id| id.request_identity.as_str()),
        native.is_some_and(|result| result.terminal),
        native.and_then(|result| result.exit).map_or_else(|| "unknown".to_owned(), |exit| exit.to_string()),
        native.map_or("unknown", |result| result.reason.as_str()),
        completion.and_then(|value| value.finished_at.as_deref()).unwrap_or("unknown"), remote.detail)
        + &remote.facts.as_ref().map_or_else(String::new, |facts| {
            if facts.identity_conflict { return "\n- result_conflict: the native request/locator/runner/project identity changed.".to_owned(); }
            facts.conflict.as_ref().map_or_else(String::new, |other| format!(
                "\n- result_conflict: contradictory native terminal {:?}, exit {:?}, reason {}, finished {}. Neither terminal authorizes credit.",
                other.result.verdict, other.result.exit, other.result.reason, other.finished_at.as_deref().unwrap_or("unknown")))
        })
}

impl AppState {
    // Called only inside the existing authenticated relay. The actor's host
    // vouches for its native caller/project; no caller supplies completion facts.
    fn read_remote_test_run_origin(
        &self, actor: &MailboxRelayPrincipal, scope: &str, run: &str,
        lease: &FederationLinkLease, authenticated: &FederationHostIdentity,
    ) -> Result<RemoteTestRunReply, ApiError> {
        actor.require_origin(authenticated)?;
        if scope.is_empty() || scope.len() > 1024 || run.is_empty()
            || run.len() > TEST_RUN_TEXT_MAX_BYTES
            || !run.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_')) {
            return Err(ApiError::bad_request("invalid native run observation selector"));
        }
        let expected = lease.with_current(self, |inner, peer| {
            if !mailbox_relay_peer_current_locked(inner, peer, peer.enrollment_generation) {
                return Err(federation_stale_link());
            }
            let row = inner.test_run_registrations.get(run).or_else(||
                inner.test_run_compacted.get(run).and_then(|row| row.registration.as_ref()));
            let Some(row) = row else { return Ok(None); };
            let Some(origin) = &row.native_origin else { return Ok(None); };
            if origin.host != peer.local || origin.project_scope.as_deref() != Some(scope)
                || row.project_id.as_deref() != Some(origin.project_id.as_str())
                || row.initiating_session_id != origin.native_session_id
                || row.owning_session_id != origin.native_session_id
            { return Err(ApiError::conflict("association_stale: native run project/origin does not match")); }
            Ok(Some(row.clone()))
        })?;
        let pending = || RemoteTestRunReply { identity: None, completion: None,
            detail: "association_stale: native registration or completion not durably available".to_owned() };
        let Some(expected) = expected else { return Ok(pending()); };
        // One selected-field SQLite snapshot proves the same original native
        // observation and registration. Raw launcher JSON is never a producer.
        let mut connection = open_sqlite_state_read_connection(self.persistence_path.as_path())
            .map_err(|_| ApiError::internal("native completion persistence is unavailable"))?;
        let transaction = connection.transaction()
            .map_err(|_| ApiError::internal("native completion snapshot is unavailable"))?;
        let metadata = read_metadata_for_fence(&transaction,
            &["testRunRegistrations", "testRunCompacted", "testRunObservations"])
            .map_err(|_| ApiError::internal("native completion read-back failed"))?;
        let Some(metadata) = metadata else { return Ok(pending()); };
        let compacted = metadata.test_run_compacted.get(run)
            .filter(|row| row.compacted && row.summary.run_id == run);
        let stored = metadata.test_run_registrations.get(run).or_else(||
            compacted.and_then(|row| row.registration.as_ref()));
        if stored != Some(&expected) { return Ok(pending()); }
        let origin = expected.native_origin.clone().expect("captured native origin");
        let Some(locator) = expected.locator_identity.clone() else { return Ok(pending()); };
        let identity = RemoteTestRunIdentity { origin, run_id: run.to_owned(),
            request_identity: expected.request_identity.clone(), locator_identity: locator,
            started_at: expected.started_at.clone() };
        let completion = expected.native_completion.as_ref().filter(|retained|
            metadata.test_run_observations.get(&retained.execution_id)
                .or_else(|| compacted.filter(|row| row.registration.as_ref() == Some(&expected))
                    .and_then(|row| {
                        // Retirement moves original executions into this same
                        // durable survivor. Never search another run's history.
                        let mut exact = row.observations.iter()
                            .filter(|row| row.execution_id == retained.execution_id);
                        exact.next().filter(|_| exact.next().is_none())
                    }))
                .is_some_and(|row| row.runtime_identity.is_some()
                    && row.summary.ended_at.is_some()
                    && row.summary.owner_session_id.as_deref() == Some(expected.owning_session_id.as_str())
                    && row.artifact.as_ref().is_some_and(|artifact| artifact.run_id == run
                        && artifact.request_identity.as_deref() == Some(expected.request_identity.as_str()))))
            .map(|retained| retained.completion.clone());
        lease.with_current(self, |inner, peer| {
            if peer.local != identity.origin.host
                || !mailbox_relay_peer_current_locked(inner, peer, peer.enrollment_generation) {
                return Err(federation_stale_link());
            }
            Ok(())
        })?;
        Ok(RemoteTestRunReply { identity: Some(identity), completion,
            detail: if expected.native_completion.is_some() { "native settlement retained" }
                else { "result_not_pass: native completion is pending" }.to_owned() })
    }

    fn remote_test_run_waits_durable(&self, wait: &TestRunWaitRecord) -> bool {
        let Ok(mut connection) = open_sqlite_state_read_connection(self.persistence_path.as_path()) else { return false; };
        let Ok(transaction) = connection.transaction() else { return false; };
        read_metadata_for_fence(&transaction, &["testRunWaits"]).ok().flatten()
            .is_some_and(|metadata| metadata.test_run_waits.iter().any(|stored|
                serde_json::to_value(stored).ok() == serde_json::to_value(wait).ok()))
    }

    fn ingest_remote_test_run_reply(
        &self, expected_wait: &TestRunWaitRecord, reference: &str, reply: RemoteTestRunReply,
        lease: &FederationLinkLease,
    ) -> Result<(), ApiError> {
        let (_, native_run) = remote_test_run_reference(reference)?
            .ok_or_else(|| ApiError::bad_request("remote run reference required"))?;
        let expected_remote = expected_wait.runs.iter().find(|label| label.run_id == reference)
            .and_then(|label| label.remote.as_ref()).ok_or_else(federation_stale_link)?;
        let Some(identity) = reply.identity else {
            self.remote_test_run_wait_pending(expected_wait, reference,
                "association_stale: native registration/completion is not durably available");
            return Ok(());
        };
        if identity.origin.host != expected_remote.host || identity.run_id != native_run
            || identity.origin.project_scope != expected_remote.project_scope
            || identity.request_identity.is_empty() || identity.request_identity.len() > 1024
            || identity.locator_identity.is_empty() || identity.locator_identity.len() > 1024
            || identity.origin.host.validate().is_err()
            || validate_federation_reference(&identity.origin.native_session_id).is_err()
            || !mailbox_relay_uuid(&identity.origin.native_creation_id)
            || reply.detail.len() > 128
        { return Err(ApiError::conflict("association_stale: native completion identity mismatch")); }
        if let Some(completion) = &reply.completion {
            completion.validate()?;
            if completion.origin.host_id != identity.origin.host.host_id
                || completion.origin.runner_session_id != identity.origin.native_session_id
                || completion.run_id != identity.run_id {
                return Err(ApiError::conflict("association_stale: native completion origin/request mismatch"));
            }
        }
        let key = remote_test_run_facts_key(&identity.origin.host, &identity.run_id);
        lease.with_current(self, |inner, peer| {
            if peer.peer != expected_remote.host
                || !mailbox_relay_peer_current_locked(inner, peer, peer.enrollment_generation)
                || !remote_test_run_wait_current(inner, expected_wait, expected_remote) {
                return Err(federation_stale_link());
            }
            let mut facts = inner.remote_test_run_facts.get(&key).cloned().unwrap_or(RemoteTestRunFacts {
                identity: identity.clone(), completion: None, conflict: None, identity_conflict: false,
            });
            if facts.identity != identity { facts.identity_conflict = true; }
            if let Some(completion) = reply.completion.as_ref().filter(|value| value.authoritative_terminal()) {
                if let Some(original) = &facts.completion {
                    if original != completion && facts.conflict.is_none() { facts.conflict = Some(completion.clone()); }
                } else if !facts.identity_conflict { facts.completion = Some(completion.clone()); }
            }
            let changed = inner.remote_test_run_facts.get(&key) != Some(&facts);
            let unconfirmed = !expected_remote.confirmed || expected_remote.facts.as_ref() != Some(&facts);
            if changed {
                inner.remote_test_run_facts.insert(key.clone(), facts);
            }
            if changed || unconfirmed {
                self.persist_internal_locked(&inner).map_err(|_| ApiError::internal("remote completion persistence failed"))?;
            }
            Ok(())
        })?;
        let Ok(mut connection) = open_sqlite_state_read_connection(self.persistence_path.as_path()) else { return Ok(()); };
        let Ok(transaction) = connection.transaction() else { return Ok(()); };
        let stored = read_metadata_for_fence(&transaction, &["remoteTestRunFacts"])
            .ok().flatten().and_then(|metadata| metadata.remote_test_run_facts.get(&key).cloned());
        lease.with_current(self, |inner, peer| {
            if peer.peer != expected_remote.host || !mailbox_relay_peer_current_locked(inner, peer, peer.enrollment_generation) {
                return Err(federation_stale_link());
            }
            if !remote_test_run_wait_current(inner, expected_wait, expected_remote)
            || stored.as_ref() != inner.remote_test_run_facts.get(&key) || stored.is_none() { return Ok(()); }
        let Some(wait) = inner.test_run_waits.iter_mut().find(|wait| wait.id == expected_wait.id) else { return Ok(()); };
        let Some(remote) = wait.runs.iter_mut().find(|label| label.run_id == reference).and_then(|label| label.remote.as_mut()) else { return Ok(()); };
        if remote.identity.as_ref().is_some_and(|prior| prior != &identity) {
            remote.detail = "result_conflict: native request/runner/project identity changed".to_owned();
        } else { remote.identity = Some(identity); remote.detail = reply.detail; }
        remote.facts = stored;
        remote.confirmed = true;
            self.commit_locked(inner).map_err(|_| ApiError::internal("remote wait persistence failed"))?;
            Ok(())
        })?;
        self.refresh_test_run_waits();
        Ok(())
    }

    fn prepare_remote_test_run_wait_scopes(&self) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let scopes = inner.test_run_waits.iter().filter_map(|wait| {
            let index = inner.find_visible_session_index(&wait.session_id)?;
            let session = &inner.sessions[index];
            let project = session.session.project_id.as_deref()?;
            Some((wait.id.clone(), project.to_owned(), session.federation_native_creation_id.clone(),
                remote_test_run_scope(&inner, project)?))
        }).collect::<Vec<_>>();
        let mut changed = false;
        for (id, project, creation, scope) in scopes {
            if let Some(wait) = inner.test_run_waits.iter_mut().find(|wait| wait.id == id) {
                for label in &mut wait.runs {
                    if let Some(remote) = label.remote.as_mut().filter(|remote|
                        remote.project_scope.is_none() && remote.project_id == project && remote.creation_id == creation) {
                        remote.project_scope = Some(scope.clone());
                        changed = true;
                    }
                }
            }
        }
        let unconfirmed = inner.test_run_waits.iter().any(|wait| wait.runs.iter().any(|label|
            label.remote.as_ref().is_some_and(|remote| remote.project_scope.is_some() && !remote.confirmed)));
        if changed || unconfirmed {
            if let Err(error) = self.persist_internal_locked(&inner) {
                eprintln!("remote test wait project scope persistence deferred: {error:#}");
            }
        }
    }

    fn remote_test_run_wait_pending(&self, expected: &TestRunWaitRecord, reference: &str, detail: &str) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(label) = expected.runs.iter().find(|label| label.run_id == reference).and_then(|label| label.remote.as_ref()) else { return; };
        if !remote_test_run_wait_current(&inner, expected, label) { return; }
        let Some(current) = inner.test_run_waits.iter_mut().find(|wait| wait.id == expected.id)
            .and_then(|wait| wait.runs.iter_mut().find(|label| label.run_id == reference))
            .and_then(|label| label.remote.as_mut()) else { return; };
        // Availability uncertainty cannot downgrade a confirmed native terminal.
        if current.detail == detail || current.facts.as_ref().is_some_and(|facts| facts.completion.is_some()) { return; }
        current.detail = detail.to_owned();
        if let Err(error) = self.commit_locked(&mut inner) {
            eprintln!("remote test wait availability persistence deferred: {error:#}");
        }
    }
}

// Joined by the existing federation supervisor. Its periodic fallback retries
// observations, never tests or imports. Each pass is a bounded round-robin batch.
#[derive(Default)]
struct RemoteTestRunWaitRecovery {
    task: Option<tokio::task::JoinHandle<()>>,
    observed_kick: u64,
    after: usize,
}

impl RemoteTestRunWaitRecovery {
    async fn tick(&mut self, app: &AppState, fallback: bool) {
        if self.task.as_ref().is_some_and(|task| !task.is_finished()) { return; }
        if let Some(task) = self.task.take() { let _ = task.await; }
        if app.federation_executor_wake_signal.is_stopping() { return; }
        let generation = app.federation_executor_wake_signal.generation.load(Ordering::SeqCst);
        if !fallback && generation == self.observed_kick { return; }
        self.observed_kick = generation;
        let after = self.after;
        self.after = self.after.wrapping_add(4);
        let app = app.clone();
        self.task = Some(tokio::spawn(async move {
            let planner = app.clone();
            let picked = run_blocking_api(move || {
                planner.prepare_remote_test_run_wait_scopes();
                let inner = planner.inner.lock().expect("state mutex poisoned");
                let total = inner.test_run_waits.iter().map(|wait|
                    wait.runs.iter().filter(|label| label.remote.is_some()).count()).sum::<usize>();
                if total == 0 { return Ok(Vec::new()); }
                let start = after % total;
                let plans = inner.test_run_waits.iter().flat_map(|wait| wait.runs.iter()
                    .filter(|label| label.remote.is_some()).map(move |label| (wait, label)));
                Ok(plans.cycle().skip(start).take(total.min(4))
                    .map(|(wait, label)| (wait.clone(), label.clone())).collect::<Vec<_>>())
            }).await.unwrap_or_default();
            for (wait, label) in picked {
                if app.federation_executor_wake_signal.is_stopping() { break; }
                let Some(remote) = label.remote.clone() else { continue; };
                let Some(scope) = remote.project_scope.clone() else { continue; };
                let Some(handle) = app.federation_peer_handle(&remote.host.host_id) else { continue; };
                let Ok(lease) = handle.channel.current_lease() else { continue; };
                let worker = app.clone();
                let check_wait = wait.clone();
                let check_remote = remote.clone();
                let session = wait.session_id.clone();
                let Ok(Some(actor)) = run_blocking_api(move || {
                    if !worker.remote_test_run_waits_durable(&check_wait) { return Ok(None); }
                    if !remote_test_run_wait_current(&worker.inner.lock().expect("state mutex poisoned"), &check_wait, &check_remote) {
                        return Ok(None);
                    }
                    worker.native_mailbox_relay_descriptor(&session).map(|descriptor| Some(descriptor.principal))
                }).await else { continue; };
                let Ok(Some((_, native_run))) = remote_test_run_reference(&label.run_id) else { continue; };
                let operation = MailboxRelayOperation::ObserveTestRun { actor, project_scope: scope, run_id: native_run };
                let reply = match tokio::time::timeout(Duration::from_secs(5),
                    app.request_mailbox_relay::<RemoteTestRunReply>(&handle, &lease, operation)).await {
                    Ok(Ok(reply)) => reply,
                    failure => {
                        let detail = if matches!(failure, Ok(Err(ref error)) if matches!(error.status, StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND)) {
                            "producer_unsupported: the origin does not expose native completion observations"
                        } else { "association_stale: authenticated native completion is unavailable; wait remains pending" };
                        let worker = app.clone();
                        let pending = wait.clone();
                        let reference = label.run_id.clone();
                        let _ = run_blocking_api(move || {
                            worker.remote_test_run_wait_pending(&pending, &reference, detail); Ok(())
                        }).await;
                        continue;
                    }
                };
                let worker = app.clone();
                let _ = run_blocking_api(move || worker.ingest_remote_test_run_reply(&wait, &label.run_id, reply, &lease)).await;
            }
        }));
    }

    async fn shutdown(self) {
        if let Some(task) = self.task { let _ = task.await; }
    }
}
