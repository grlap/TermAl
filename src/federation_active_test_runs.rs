// A presentation-only view of the origin's already materialized run projection.
// This read never opens a run directory or touches mailbox or check custody.

const ACTIVE_TEST_RUN_ROW_LIMIT: usize = 20;
const ACTIVE_TEST_RUN_ID_BYTES: usize = 128;
const ACTIVE_TEST_RUN_LABEL_BYTES: usize = 96;
const ACTIVE_TEST_RUN_TIMESTAMP_BYTES: usize = 40;

fn active_test_run_origin_enabled(inner: &StateInner) -> bool {
    inner.preferences.federation.enabled
        && inner.preferences.federation.role == FederationRole::Satellite
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederatedActiveTestRunRow {
    run_id: String,
    state: TestRunState,
    project: Option<String>,
    owner: Option<String>,
    started_at: Option<String>,
}

#[derive(Default)]
struct ActiveTestRunProjection {
    runs: Vec<FederatedActiveTestRunRow>,
    truncated: bool,
}

fn active_test_run_label(value: &str) -> Option<&str> {
    let mut end = value.len().min(ACTIVE_TEST_RUN_LABEL_BYTES);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    let label = value[..end].trim();
    (!label.is_empty() && !label.chars().any(char::is_control)).then_some(label)
}

fn active_test_run_id_valid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= ACTIVE_TEST_RUN_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn active_test_run_timestamp_valid(value: &str) -> bool {
    value.len() <= ACTIVE_TEST_RUN_TIMESTAMP_BYTES
        && chrono::DateTime::parse_from_rfc3339(value).is_ok()
}

impl ActiveTestRunProjection {
    // The canonical projection has already sorted and merged launcher and
    // observed execution identities. Never reclassify a run in this view.
    fn include(&mut self, run: &TestRunSummary, source: &TestRunProjectionCut) {
        if self.runs.len() == ACTIVE_TEST_RUN_ROW_LIMIT || !active_test_run_id_valid(&run.run_id) {
            self.truncated = true;
            return;
        }
        self.runs.push(FederatedActiveTestRunRow {
            run_id: run.run_id.clone(),
            state: TestRunState::Running,
            project: run
                .project_id
                .as_deref()
                .and_then(|id| source.projects.iter().find(|(key, _)| key == id)?.1.clone()),
            owner: run.owner_session_id.as_deref().and_then(|id| {
                let owner = source.projection_session_by_id(id)?;
                (owner.local && !owner.hidden)
                    .then(|| active_test_run_label(owner.name))
                    .flatten()
                    .map(str::to_owned)
            }),
            started_at: run
                .started_at
                .as_ref()
                .filter(|stamp| active_test_run_timestamp_valid(stamp))
                .cloned(),
        });
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederatedActiveTestRunsReply {
    version: u16,
    origin: FederationHostIdentity,
    snapshot_version: u64,
    as_of: Option<String>,
    ready: bool,
    truncated: bool,
    runs: Vec<FederatedActiveTestRunRow>,
}

fn active_test_runs_unavailable() -> ApiError {
    ApiError::from_status(
        StatusCode::SERVICE_UNAVAILABLE,
        "remote Test Runs snapshot unavailable",
    )
}

impl FederationLinkLease {
    // Take the same link -> StateInner cut as admission, without ever waiting
    // for a writer. No stale or unconfirmed cut can authorize a snapshot.
    fn try_with_active_test_runs_authority<T>(
        &self,
        read: impl FnOnce(Option<(&StateInner, FederationPeerSnapshot)>) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        let channel = &self.channel;
        let link = channel.authenticated_link_state(true)?;
        let incarnation = Some(self.incarnation.as_str());
        if link.incarnation.as_deref() != incarnation
            || link.recorded_incarnation.as_deref() != incarnation
        {
            return Err(federation_stale_link());
        }
        let Some(app) = channel.app.as_ref() else {
            #[cfg(test)]
            return read(None);
            #[cfg(not(test))]
            return Err(active_test_runs_unavailable());
        };
        let basis = channel
            .basis
            .try_read()
            .map_err(|_| active_test_runs_unavailable())?;
        let inner = app
            .inner
            .try_lock_now()
            .ok_or_else(active_test_runs_unavailable)?;
        if app.inner.inner.is_poisoned() {
            return Err(active_test_runs_unavailable());
        }
        let pair =
            app.refresh_federation_transport_snapshot_locked(&inner, &basis, &self.incarnation)?;
        app.ensure_federation_admission_allowed_locked(&inner)?;
        read(Some((&inner, pair)))
    }
}

async fn serve_federated_active_test_runs(
    axum::Extension(peer): axum::Extension<FederationAuthenticatedPeer>,
) -> Result<Response, ApiError> {
    let lease = peer.lease.ok_or_else(federation_stale_link)?;
    let channel = &lease.channel;
    let _read = channel
        .active_test_runs_read
        .try_acquire()
        .map_err(|_| active_test_runs_unavailable())?;
    let reply = lease.try_with_active_test_runs_authority(|current| {
        let (inner, pair) = current.ok_or_else(active_test_runs_unavailable)?;
        if pair.local_role != FederationRole::Satellite || pair.peer != pair.central {
            return Err(ApiError::from_status(
                StatusCode::FORBIDDEN,
                "active Test Runs are available only to this host's enrolled Central",
            ));
        }
        let mut reply = FederatedActiveTestRunsReply {
            version: 1,
            origin: pair.local,
            snapshot_version: 0,
            as_of: None,
            ready: false,
            truncated: false,
            runs: Vec::new(),
        };
        if inner.test_runs.scanned_once {
            let cache = inner
                .state_projection_cache
                .try_lock()
                .map_err(|_| active_test_runs_unavailable())?;
            let projection = cache
                .test_runs
                .as_ref()
                .ok_or_else(active_test_runs_unavailable)?;
            // A cut may have been captured but not yet rendered by the worker.
            // Reading must not force a history projection in that case.
            if !projection.cut.active_peer_view || !projection.cut.projection_index().scanned_once {
                return Err(active_test_runs_unavailable());
            }
            let value = projection
                .value
                .get()
                .ok_or_else(active_test_runs_unavailable)?;
            reply.snapshot_version = projection.version;
            reply.as_of = Some(projection.captured_at.clone());
            reply.ready = true;
            reply.truncated = value.active.truncated;
            reply.runs.clone_from(&value.active.runs);
        }
        Ok(reply)
    })?;
    let bytes = serde_json::to_vec(&reply).map_err(|_| active_test_runs_unavailable())?;
    if bytes.len() > FEDERATION_ACTIVE_TEST_RUNS_RESPONSE_BYTES {
        return Err(active_test_runs_unavailable());
    }
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response())
}

impl FederationRpcClient {
    // The operator-facing selector uses this in the next slice.
    #[cfg_attr(not(test), allow(dead_code))]
    async fn active_test_runs(
        &self,
        lease: &FederationLinkLease,
    ) -> Result<FederatedActiveTestRunsReply, ApiError> {
        if self.channel.snapshot.local_role != FederationRole::Central {
            return Err(ApiError::from_status(
                StatusCode::FORBIDDEN,
                "remote active Test Runs require the enrolled Central",
            ));
        }
        let read = self
            .channel
            .active_test_runs_read
            .clone()
            .try_acquire_owned()
            .map_err(|_| active_test_runs_unavailable())?;
        let client = self.clone();
        let request_lease = lease.clone();
        // An operator may change selection or close the panel while the
        // counter is outstanding. Keep the bounded transport and its permit
        // alive so its normal settlement cannot be skipped by that drop.
        let reply: FederatedActiveTestRunsReply = tokio::spawn(async move {
            let _read = read;
            client
                .request(FederationRpcRoute::ActiveTestRuns(request_lease))
                .await
        })
        .await
        .map_err(|_| active_test_runs_unavailable())??;
        let mut run_ids = HashSet::new();
        if reply.version != 1
            || reply.origin != self.channel.snapshot.peer
            || reply.runs.len() > ACTIVE_TEST_RUN_ROW_LIMIT
            || reply.ready
                != reply
                    .as_of
                    .as_deref()
                    .is_some_and(active_test_run_timestamp_valid)
            || (reply.ready && reply.snapshot_version == 0)
            || (!reply.ready
                && (reply.snapshot_version != 0
                    || reply.as_of.is_some()
                    || reply.truncated
                    || !reply.runs.is_empty()))
            || reply.runs.iter().any(|row| {
                row.state != TestRunState::Running
                    || !active_test_run_id_valid(&row.run_id)
                    || !run_ids.insert(row.run_id.as_str())
                    || row
                        .project
                        .as_deref()
                        .is_some_and(|label| Some(label) != active_test_run_label(label))
                    || row
                        .owner
                        .as_deref()
                        .is_some_and(|label| Some(label) != active_test_run_label(label))
                    || row
                        .started_at
                        .as_deref()
                        .is_some_and(|stamp| !active_test_run_timestamp_valid(stamp))
            })
        {
            return Err(active_test_runs_unavailable());
        }
        lease.try_with_active_test_runs_authority(|_| Ok(()))?;
        Ok(reply)
    }
}
