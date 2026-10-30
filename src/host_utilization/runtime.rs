// Owns one sampler, bounded peer refreshes and cached HTTP projections.
// This observation runtime never invokes executor or test-run operations.
use super::*;

const SAMPLER_SHUTDOWN_GRACE: Duration = Duration::from_millis(250);

struct SamplerOccupancy(Arc<Cache>);
impl Drop for SamplerOccupancy {
    fn drop(&mut self) {
        self.0.sampler_running.store(false, Ordering::SeqCst);
    }
}

pub(crate) struct Runtime {
    stop: mpsc::Sender<()>,
    sampler: std::thread::JoinHandle<()>,
    peer_stop: tokio::sync::watch::Sender<bool>,
    peers: tokio::task::JoinHandle<()>,
}
impl Runtime {
    pub(crate) fn start(app: AppState) -> std::io::Result<Self> {
        Self::start_with_sampler(app, sample_loop)
    }
    pub(super) fn start_with_sampler(
        app: AppState,
        run: impl FnOnce(AppState, mpsc::Receiver<()>) + Send + 'static,
    ) -> std::io::Result<Self> {
        if app
            .host_utilization
            .sampler_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "host utilization sampler already running",
            ));
        }
        let (stop, stopped) = mpsc::channel();
        let sampler_app = app.clone();
        let cache = app.host_utilization.clone();
        let sampler = std::thread::Builder::new()
            .name("host-utilization".to_owned())
            .spawn(move || {
                // Only the actual owner releases occupancy, after its final
                // possible publication, including when a probe unwinds.
                let _occupancy = SamplerOccupancy(cache);
                run(sampler_app, stopped);
            })
            .inspect_err(|_| {
                app.host_utilization
                    .sampler_running
                    .store(false, Ordering::SeqCst)
            })?;
        let (peer_stop, peer_stopped) = tokio::sync::watch::channel(false);
        let peers = tokio::spawn(refresh_loop(app, peer_stopped));
        Ok(Self {
            stop,
            sampler,
            peer_stop,
            peers,
        })
    }
    pub(crate) async fn shutdown(self) {
        let _ = self.stop.send(());
        self.peer_stop.send_replace(true);
        let _ = self.peers.await;
        // A native call cannot be cancelled. Do not let it prevent the host's
        // persistence drain, or leave an unbounded Tokio blocking task behind.
        let completed = tokio::time::timeout(SAMPLER_SHUTDOWN_GRACE, async {
            while !self.sampler.is_finished() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .is_ok();
        if completed {
            let _ = self.sampler.join();
        } else {
            // Dropping the handle detaches this exact thread; its exit guard
            // keeps any replacement refused until the blocked probe returns.
            eprintln!("utilization> sampler still stopping after shutdown grace");
        }
    }
}

struct Configuration {
    host: Option<FederationHostIdentity>,
    central: bool,
    peers: Vec<Peer>,
    total: usize,
    complete: bool,
    paths: native::VolumePaths,
}
fn configuration(app: &AppState, extra_paths: &[PathBuf]) -> Option<Configuration> {
    // No waiting for StateInner and no native work under its guard. Projects
    // and registered local runs provide runner checkout roots, not a Git scan.
    let inner = app.inner.try_lock_now()?;
    let central = inner.preferences.federation.role == FederationRole::Central;
    let total = inner
        .federation
        .peers
        .values()
        .filter(|peer| peer.membership != FederationMembershipState::Revoked)
        .count();
    let peers = inner
        .federation
        .peers
        .values()
        .filter(|peer| peer.membership != FederationMembershipState::Revoked)
        .take(MAX_HOSTS)
        .map(|peer| Peer {
            identity: peer.identity.clone(),
            os: peer.platform.clone(),
            architecture: peer.arch.clone(),
            enrollment_generation: peer.enrollment_generation,
            transport_generation: peer.transport_generation,
            connected: inner.preferences.federation.enabled
                && inner.federation_durable
                && !inner.federation_continuity_lost
                && inner.federation.links.contains_key(&peer.identity.host_id),
        })
        .collect();
    let required = BTreeSet::from([
        PathBuf::from(&app.default_workdir),
        app.persistence_path.parent()?.to_path_buf(),
    ])
    .into_iter()
    .collect::<Vec<_>>();
    let mut paths = BTreeSet::<PathBuf>::new();
    let mut omitted_at_least = 0;
    for (index, path) in extra_paths
        .iter()
        .map(PathBuf::as_path)
        .chain(
            inner
                .projects
                .iter()
                .filter(|p| p.remote_id == default_local_remote_id())
                .map(|p| FsPath::new(&p.root_path)),
        )
        .chain(
            inner
                .test_runs
                .entries
                .iter()
                .filter(|entry| entry.summary.state == TestRunState::Running)
                .map(|entry| FsPath::new(&entry.summary.worktree)),
        )
        .enumerate()
    {
        if index >= MAX_CONFIGURATION_INPUTS {
            omitted_at_least += 1;
            break;
        }
        if path.as_os_str().len() > 32768 {
            omitted_at_least += 1;
            continue;
        }
        if required.iter().any(|required| required.as_path() == path) || paths.contains(path) {
            continue;
        }
        if paths.len() + required.len() >= MAX_VOLUME_PATHS {
            omitted_at_least += 1;
            break;
        }
        paths.insert(path.to_path_buf());
    }
    Some(Configuration {
        host: inner.federation.local.clone(),
        central,
        peers,
        total,
        complete: total <= MAX_HOSTS,
        paths: native::VolumePaths {
            required,
            additional: paths.into_iter().collect(),
            omitted_at_least,
        },
    })
}
fn sample_loop(app: AppState, stop: mpsc::Receiver<()>) {
    let mut collector = native::Collector::new();
    sample_loop_with_probe(app, stop, move |host, paths| collector.sample(host, paths));
}

pub(super) fn sample_loop_with_probe(
    app: AppState,
    stop: mpsc::Receiver<()>,
    mut collect: impl FnMut(Option<FederationHostIdentity>, &native::VolumePaths) -> Snapshot,
) {
    // Read configuration once, never shell out or inspect directory contents.
    let extra = std::env::var_os("TERMAL_UTILIZATION_VOLUMES")
        .map(|value| {
            std::env::split_paths(&value)
                .take(MAX_CONFIGURATION_INPUTS + 1)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    loop {
        if !matches!(stop.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            break;
        }
        if let Some(config) = configuration(&app, &extra) {
            let roster_observed = std::time::Instant::now();
            // Publish before probing; the refresh loop also updates the roster
            // independently if a native probe blocks this single sampler.
            publish_roster(&app.host_utilization, &config, roster_observed);
            let sample = collect(config.host.clone(), &config.paths);
            // A stop received inside a blocked native probe ends this round;
            // it must not start another probe or publish after shutdown.
            if !matches!(stop.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                break;
            }
            let stored = Stored {
                snapshot: sample,
                observed: std::time::Instant::now(),
                received_at: utc_now(),
            };
            if serde_json::to_vec(&stored.snapshot)
                .is_ok_and(|bytes| bytes.len() <= RESPONSE_BYTES - 256)
            {
                let mut image = app
                    .host_utilization
                    .image
                    .write()
                    .expect("utilization cache poisoned");
                if image.local_identity == stored.snapshot.host {
                    image.local = Some(stored);
                }
            }
        }
        match stop.recv_timeout(SAMPLE_INTERVAL) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}
fn same_peer(left: &Peer, right: &Peer) -> bool {
    left.identity == right.identity
        && left.enrollment_generation == right.enrollment_generation
        && left.transport_generation == right.transport_generation
}
fn publish_roster(cache: &Cache, config: &Configuration, now: std::time::Instant) {
    let mut image = cache.image.write().expect("utilization cache poisoned");
    let mut previous = std::mem::take(&mut image.peers);
    for peer in &config.peers {
        let old = previous
            .remove(&peer.identity.host_id)
            .filter(|old| same_peer(&old.peer, peer));
        image.peers.insert(
            peer.identity.host_id.clone(),
            PeerSample {
                peer: peer.clone(),
                sample: old.as_ref().and_then(|old| old.sample.clone()),
                reason: old.and_then(|old| old.reason),
            },
        );
    }
    image.central = Some(config.central);
    image.local_identity = config.host.clone();
    if image
        .local
        .as_ref()
        .is_some_and(|sample| sample.snapshot.host != config.host)
    {
        image.local = None;
    }
    image.roster_total = config.total + 1;
    image.roster_complete = config.complete;
    image.roster_observed = Some(now);
}

async fn refresh_loop(app: AppState, mut stop: tokio::sync::watch::Receiver<bool>) {
    let mut jobs = tokio::task::JoinSet::<(Peer, Result<Snapshot, Reason>)>::new();
    let mut owners = HashMap::<tokio::task::Id, Peer>::new();
    let mut in_flight = HashSet::<String>::new();
    let mut attempted = HashMap::<String, std::time::Instant>::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            Some(result) = jobs.join_next_with_id(), if !jobs.is_empty() => {
                match result {
                    Ok((id, (peer, result))) => {
                        owners.remove(&id);
                        in_flight.remove(&peer.identity.host_id);
                        publish_peer(&app.host_utilization, &peer, result, std::time::Instant::now());
                    }
                    Err(error) => if let Some(peer) = owners.remove(&error.id()) {
                        in_flight.remove(&peer.identity.host_id);
                        publish_peer(&app.host_utilization, &peer, Err(Reason::ProbeFailed), std::time::Instant::now());
                    }
                }
            }
            _ = tick.tick() => {
                if *stop.borrow() { break; }
                if let Some(config) = configuration(&app, &[]) {
                    publish_roster(&app.host_utilization, &config, std::time::Instant::now());
                }
                let peers = {
                    let Ok(image) = app.host_utilization.image.try_read() else { continue };
                    if image.central != Some(true) { continue; }
                    image.peers.values().map(|row| row.peer.clone()).collect::<Vec<_>>()
                };
                attempted.retain(|id, _| peers.iter().any(|peer| &peer.identity.host_id == id));
                let now = std::time::Instant::now();
                let mut due = peers.into_iter().filter(|peer| peer.connected
                    && !in_flight.contains(&peer.identity.host_id)
                    && attempted.get(&peer.identity.host_id).is_none_or(|last| now.saturating_duration_since(*last) >= REFRESH_INTERVAL))
                    .collect::<Vec<_>>();
                // Oldest attempted peer first; a large roster cannot starve
                // behind the first four entries on every coalesced tick.
                due.sort_by_key(|peer| attempted.get(&peer.identity.host_id).copied());
                for peer in due.into_iter().take(4usize.saturating_sub(jobs.len())) {
                    in_flight.insert(peer.identity.host_id.clone());
                    attempted.insert(peer.identity.host_id.clone(), now);
                    let handle = app.federation_peer_handle(&peer.identity.host_id);
                    let owner = peer.clone();
                    let task = jobs.spawn(async move {
                        let result = match handle {
                            Some(handle) => match handle.channel.current_lease() {
                                Ok(lease) => handle.rpc.host_utilization(&lease).await,
                                Err(_) => Err(Reason::PeerDisconnected),
                            },
                            None => Err(Reason::PeerDisconnected),
                        };
                        (peer, result)
                    });
                    owners.insert(task.id(), owner);
                }
            }
        }
    }
    // Requests own their bounded transport timeout. Do not cancel a future
    // with an allocated authenticated counter at shutdown.
    while jobs.join_next().await.is_some() {}
}
pub(super) fn publish_peer(
    cache: &Cache,
    peer: &Peer,
    result: Result<Snapshot, Reason>,
    now: std::time::Instant,
) {
    let mut image = cache.image.write().expect("utilization cache poisoned");
    let Some(row) = image
        .peers
        .get_mut(&peer.identity.host_id)
        .filter(|row| same_peer(&row.peer, peer))
    else {
        return;
    };
    match result {
        Ok(snapshot) if snapshot.validate(&peer.identity) => {
            row.sample = Some(Stored {
                snapshot,
                observed: now,
                received_at: utc_now(),
            });
            row.reason = None;
        }
        Ok(_) => row.reason = Some(Reason::IdentityChanged),
        Err(reason) => row.reason = Some(reason),
    }
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/host-utilization", get(local))
        .route("/api/federation/host-utilization", get(aggregate))
}
async fn local(State(app): State<AppState>) -> Result<Response, ApiError> {
    let started = std::time::Instant::now();
    let snapshot = app.host_utilization.local(started)?;
    cached_response(
        &snapshot,
        started,
        Duration::from_millis(250),
        RESPONSE_BYTES,
    )
}
async fn aggregate(State(app): State<AppState>) -> Result<Response, ApiError> {
    let started = std::time::Instant::now();
    let snapshot = app.host_utilization.aggregate(started)?;
    cached_response(
        &snapshot,
        started,
        Duration::from_secs(1),
        RESPONSE_BYTES * (MAX_HOSTS + 1) + 64 * 1024,
    )
}
pub(crate) async fn serve_pair(
    axum::Extension(peer): axum::Extension<FederationAuthenticatedPeer>,
) -> Result<Response, ApiError> {
    let started = std::time::Instant::now();
    let lease = peer.lease.ok_or_else(federation_stale_link)?;
    let app = lease
        .channel
        .app
        .as_ref()
        .ok_or_else(|| unavailable("host utilization unavailable"))?;
    let snapshot = app.host_utilization.local(std::time::Instant::now())?;
    lease.with_incarnation(|| {
        if snapshot.host.as_ref() != Some(&lease.channel.snapshot.local) {
            return Err(unavailable(
                "host utilization identity has not been sampled",
            ));
        }
        Ok(())
    })?;
    cached_response(
        &snapshot,
        started,
        Duration::from_millis(250),
        RESPONSE_BYTES,
    )
}
fn cached_response<T: Serialize>(
    value: &T,
    started: std::time::Instant,
    budget: Duration,
    limit: usize,
) -> Result<Response, ApiError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| unavailable("host utilization encoding unavailable"))?;
    if bytes.len() > limit || started.elapsed() > budget {
        return Err(unavailable(
            "host utilization cached response budget exceeded",
        ));
    }
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response())
}
impl FederationRpcClient {
    async fn host_utilization(&self, lease: &FederationLinkLease) -> Result<Snapshot, Reason> {
        let started = std::time::Instant::now();
        let mut snapshot: Snapshot = self
            .request(FederationRpcRoute::HostUtilization(lease.clone()))
            .await
            .map_err(|_| Reason::PeerUnavailable)?;
        // Conservatively charge the complete local round trip, never the
        // difference between two hosts' unrelated wall clocks.
        snapshot.age(started.elapsed(), false);
        Ok(snapshot)
    }
}
