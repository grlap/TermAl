// Owns: the remote wake OUTCOME that central learns from the receiver. The
// satellite answers an authenticated outcome query read-only, from its own
// journal and the root's queue and delivery state
// (federation_executor_operation_view.rs). Central's wake drain owner runs one
// polling pass at a time, in its own task off the dispatch path, over settled
// remote wakes, with backoff and a bounded age and attempt count, and keeps the
// latest verified answer in its own table. Message details show
// that answer; with none, the wake is pending, never "not woken".
// Does not own the immutable domain append receipt, the immutable central
// ownership mirror (federation_executor_mirrors.rs) or the receiver's journal,
// all of which are unchanged. New file.

const FEDERATION_EXECUTOR_OUTCOME_ROUTE: &str = "/federation/v1/executor/outcome";

// Central's latest verified receiver answer for one settled remote wake. The
// row is created when the wake settles (`first_seen_at`), so the age bound
// counts from settlement. A terminal answer is never replaced. Polling stops
// at a terminal answer, after FEDERATION_EXECUTOR_OUTCOME_MAX_ATTEMPTS
// attempts, or once the row is older than FEDERATION_EXECUTOR_OUTCOME_MAX_AGE.
const FEDERATION_EXECUTOR_OUTCOMES_SCHEMA_SQL: &str = "
    CREATE TABLE federation_executor_outcomes (
      central_host_id TEXT NOT NULL CHECK (length(central_host_id) BETWEEN 1 AND 256),
      operation_key TEXT NOT NULL CHECK (length(operation_key) BETWEEN 1 AND 1024),
      first_seen_at TEXT NOT NULL,
      last_outcome TEXT CHECK (last_outcome IS NULL OR last_outcome IN (
        'received','queued','queuedBehindActiveTurn','heldBehindPausedQueue','unknown',
        'woken','rejected','handedOffUnconfirmed','mailboxProcessed')),
      last_observed_at TEXT,
      terminal INTEGER NOT NULL CHECK (terminal IN (0, 1)),
      attempts INTEGER NOT NULL CHECK (attempts >= 0),
      next_poll_at TEXT NOT NULL,
      PRIMARY KEY (central_host_id, operation_key),
      CHECK ((last_outcome IS NULL) = (last_observed_at IS NULL)),
      CHECK (terminal = 0 OR last_outcome IN ('woken','rejected','handedOffUnconfirmed','mailboxProcessed')),
      FOREIGN KEY (central_host_id, operation_key)
        REFERENCES federation_executor_operations(central_host_id, operation_key) ON DELETE CASCADE
    );
    CREATE INDEX federation_executor_outcomes_due
      ON federation_executor_outcomes(terminal, next_poll_at);
";

// With the 60 s backoff cap, 64 attempts span about an hour, so the age bound,
// not the attempt bound, normally ends polling of a long-queued wake.
const FEDERATION_EXECUTOR_OUTCOME_MAX_ATTEMPTS: u32 = 64;
const FEDERATION_EXECUTOR_OUTCOME_MAX_AGE: Duration = Duration::from_secs(60 * 60);
const FEDERATION_EXECUTOR_OUTCOME_POLL_BATCH: usize = 8;
// A pass starts no new poll after this; each poll's RPC keeps its transport
// bound. Passes run in their own task, never on the wake dispatch path.
const FEDERATION_EXECUTOR_OUTCOME_PASS_BUDGET: Duration = Duration::from_secs(2);

/// The receiver's answer for one remote wake.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum FederationExecutorOutcome {
    /// Durably received; not yet queued on the root.
    Received,
    /// Queued on the root, or its hand-off is starting; nothing holds it.
    Queued,
    /// Queued while the root runs another turn.
    QueuedBehindActiveTurn,
    /// Queued while the root's queue is paused or the root is stopping.
    HeldBehindPausedQueue,
    /// No longer queued, with no retained evidence of what became of it.
    Unknown,
    /// The root's runtime accepted the wake.
    Woken,
    /// The wake's delivery definitely failed: refused by the runtime, or
    /// rejected before anything was sent to it.
    Rejected,
    /// The wake was handed to the runtime and its outcome is unknown.
    HandedOffUnconfirmed,
    /// The mail was processed on the root without this wake being accepted,
    /// for example by an already running turn's continuation, and this wake,
    /// once queued, is no longer queued, pending or handing off.
    MailboxProcessed,
}

impl FederationExecutorOutcome {
    const ALL: [Self; 9] = [Self::Received, Self::Queued, Self::QueuedBehindActiveTurn,
        Self::HeldBehindPausedQueue, Self::Unknown, Self::Woken, Self::Rejected,
        Self::HandedOffUnconfirmed, Self::MailboxProcessed];

    fn wire(self) -> &'static str {
        match self {
            Self::Received => "received",
            Self::Queued => "queued",
            Self::QueuedBehindActiveTurn => "queuedBehindActiveTurn",
            Self::HeldBehindPausedQueue => "heldBehindPausedQueue",
            Self::Unknown => "unknown",
            Self::Woken => "woken",
            Self::Rejected => "rejected",
            Self::HandedOffUnconfirmed => "handedOffUnconfirmed",
            Self::MailboxProcessed => "mailboxProcessed",
        }
    }

    fn from_wire(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|outcome| outcome.wire() == value)
    }

    fn terminal(self) -> bool {
        matches!(self, Self::Woken | Self::Rejected | Self::HandedOffUnconfirmed | Self::MailboxProcessed)
    }

    fn describe(self) -> &'static str {
        match self {
            Self::Received => "received by the remote host, not yet queued",
            Self::Queued => "queued on the remote root",
            Self::QueuedBehindActiveTurn => "queued behind an active turn on the remote root",
            Self::HeldBehindPausedQueue => "held behind the remote root's paused queue",
            Self::Unknown => "no longer queued on the remote root; state unknown",
            Self::Woken => "remote session woken",
            Self::Rejected => "remote wake delivery rejected",
            Self::HandedOffUnconfirmed => "handed to the remote runtime; outcome unconfirmed",
            Self::MailboxProcessed => "mail processed on the remote root without this wake",
        }
    }

    /// The answer the receiver's view supports, most definite first. Only
    /// positive runtime acceptance of this wake is reported as woken.
    fn from_view(view: &FederationExecutorOperationView) -> Self {
        if view.receipt == Some(PromptDeliveryDisposition::Accepted) || view.runtime_acknowledged
            || view.stage == "runtime_accepted"
        {
            return Self::Woken;
        }
        match view.receipt {
            Some(PromptDeliveryDisposition::Rejected) => return Self::Rejected,
            Some(PromptDeliveryDisposition::Ambiguous) => return Self::HandedOffUnconfirmed,
            _ => {}
        }
        let handing_off = view.receipt == Some(PromptDeliveryDisposition::HandingOff);
        let queued = view.queue_position.is_some() || handing_off;
        // Not yet associated with the root's queue: activation or recovery may
        // still queue it, whatever mailbox evidence says.
        let associated = view.queue_entry_id.is_some();
        // Mailbox evidence without runtime acceptance is final only once this
        // wake can no longer run: a wake not yet queued, or still queued,
        // pending or handing off after the ACK, may yet be accepted, so it is
        // classified by its queue.
        if view.stage == "processed" && associated && !queued
            && view.receipt != Some(PromptDeliveryDisposition::Pending)
        {
            // A hand-off attempt with no known result may have been accepted.
            return if view.effect_state == "unknown" { Self::HandedOffUnconfirmed } else { Self::MailboxProcessed };
        }
        if !queued {
            return if !associated { Self::Received } else { Self::Unknown };
        }
        if view.root_paused || view.root_stopping {
            return Self::HeldBehindPausedQueue;
        }
        // A hand-off in flight is this wake's own turn starting, not another.
        if !handing_off && matches!(view.root_status,
            Some(SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping))
        {
            return Self::QueuedBehindActiveTurn;
        }
        Self::Queued
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationExecutorOutcomeRequest {
    protocol_version: u16,
    operation: FederationExecutorOperation,
    payload_digest: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationExecutorOutcomeReport {
    protocol_version: u16,
    operation_key: String,
    payload_digest: String,
    outcome: FederationExecutorOutcome,
}

/// Central's retained answer for one wake.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FederationExecutorOutcomeRow {
    first_seen_at: String,
    last_outcome: Option<FederationExecutorOutcome>,
    last_observed_at: Option<String>,
    terminal: bool,
    attempts: u32,
}

impl FederationExecutorOutcomeRow {
    /// Whether polling has ended for this row without a terminal answer.
    fn polling_ended(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        let cutoff = federation_executor_outcome_cutoff(now);
        !self.terminal && (self.attempts >= FEDERATION_EXECUTOR_OUTCOME_MAX_ATTEMPTS
            || self.first_seen_at < cutoff)
    }
}

fn federation_executor_outcome_backoff(attempts: u32) -> Duration {
    Duration::from_secs((2u64 << attempts.min(5)).min(60))
}

/// The one timestamp format of this table; rows compare lexicographically.
fn federation_executor_outcome_time(time: chrono::DateTime<chrono::Utc>) -> String {
    time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn federation_executor_outcome_cutoff(now: chrono::DateTime<chrono::Utc>) -> String {
    federation_executor_outcome_time(now - chrono::Duration::from_std(FEDERATION_EXECUTOR_OUTCOME_MAX_AGE)
        .expect("bounded outcome age"))
}

/// How message details show a settled remote wake's outcome at `now`.
fn federation_executor_outcome_cause(
    row: Option<&FederationExecutorOutcomeRow>, now: chrono::DateTime<chrono::Utc>,
) -> String {
    const OWNED: &str = "remote receiver durably owns wake recovery";
    let Some(row) = row else { return format!("{OWNED}; wake outcome pending"); };
    let polled = if row.polling_ended(now) { "; no longer polled" } else { "" };
    match (row.last_outcome, row.last_observed_at.as_deref()) {
        (Some(outcome), Some(at)) if row.terminal => format!("{} at {at} (receiver evidence)", outcome.describe()),
        (Some(outcome), Some(at)) =>
            format!("{OWNED}; wake outcome pending; last seen: {} at {at}{polled}", outcome.describe()),
        _ => format!("{OWNED}; wake outcome pending{polled}"),
    }
}

/// Creates the outcome row of a wake that settles now, in the settling
/// transaction, so its age bound counts from settlement. Existing rows stay.
fn seed_federation_executor_outcome(
    connection: &rusqlite::Connection, operation: &FederationExecutorOperation, now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let at = federation_executor_outcome_time(now);
    connection.execute(
        "INSERT OR IGNORE INTO federation_executor_outcomes
           (central_host_id,operation_key,first_seen_at,last_outcome,last_observed_at,terminal,attempts,next_poll_at)
         VALUES (?1,?2,?3,NULL,NULL,0,0,?3)",
        rusqlite::params![operation.central_host_id, operation.operation_key, at],
    )?;
    Ok(())
}

impl MailboxStore {
    fn federation_executor_outcome_on(
        &self, connection: &rusqlite::Connection, central_host_id: &str, operation_key: &str,
    ) -> Result<Option<FederationExecutorOutcomeRow>> {
        connection.query_row(
            "SELECT first_seen_at,last_outcome,last_observed_at,terminal,attempts FROM federation_executor_outcomes
             WHERE central_host_id=?1 AND operation_key=?2",
            rusqlite::params![central_host_id, operation_key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?, row.get::<_, i64>(3)?, row.get::<_, u32>(4)?)),
        ).optional()?.map(|(first_seen_at, outcome, observed, terminal, attempts)| {
            let last_outcome = outcome.map(|value| FederationExecutorOutcome::from_wire(&value)
                .ok_or_else(|| anyhow!("unknown retained remote wake outcome"))).transpose()?;
            Ok(FederationExecutorOutcomeRow { first_seen_at, last_outcome, last_observed_at: observed,
                terminal: terminal == 1, attempts })
        }).transpose()
    }

    /// Open outcome rows on reachable `hosts` that are due at `now`, within
    /// the bounded age and attempts, soonest first. The scan reads only the
    /// outcome table's due index, never settled wake history. Hosts that are
    /// unreachable are not selected, so they never occupy a pass.
    fn federation_executor_outcome_candidates(
        &self, central_host_id: &str, hosts: &[String], now: chrono::DateTime<chrono::Utc>, limit: usize,
    ) -> Result<Vec<FederationExecutorOperation>> {
        if hosts.is_empty() { return Ok(Vec::new()); }
        let connection = self.connection()?;
        let keys = connection.prepare(
            "SELECT x.operation_key FROM federation_executor_outcomes x
             JOIN federation_executor_operations o
               ON o.central_host_id = x.central_host_id AND o.operation_key = x.operation_key
             WHERE x.terminal = 0 AND x.next_poll_at <= ?1 AND x.central_host_id = ?2
               AND x.attempts < ?3 AND x.first_seen_at >= ?4 AND o.command_kind = 'wake'
               AND o.target_host_id IN (SELECT value FROM json_each(?5))
             ORDER BY x.next_poll_at, x.operation_key LIMIT ?6",
        )?.query_map(rusqlite::params![
            federation_executor_outcome_time(now), central_host_id, FEDERATION_EXECUTOR_OUTCOME_MAX_ATTEMPTS,
            federation_executor_outcome_cutoff(now), serde_json::to_string(hosts)?, limit as u64,
        ], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        keys.into_iter()
            .filter_map(|key| federation_executor_read(&connection, central_host_id, &key).transpose())
            .map(|record| record.map(|record| record.operation))
            .collect()
    }

    /// Records one poll of `operation`: its verified `outcome`, or none when
    /// the receiver gave no usable answer. A terminal answer is never
    /// replaced. `authorize` runs last and is held through COMMIT for an
    /// answer, so evidence is stored only under current authority; when it
    /// refuses, the answer is dropped but the attempt and its backoff are
    /// still recorded, so a refused wake cannot be asked again every pass.
    fn record_federation_executor_outcome<G>(
        &self, operation: &FederationExecutorOperation, outcome: Option<FederationExecutorOutcome>,
        now: chrono::DateTime<chrono::Utc>, authorize: impl FnOnce() -> Result<G>,
    ) -> Result<()> {
        match self.record_federation_executor_outcome_once(operation, outcome, now, authorize) {
            Err(error) if outcome.is_some() => {
                // Keep the original cause even when recording the attempt fails too.
                if let Err(retry) = self.record_federation_executor_outcome_once(operation, None, now, || Ok(())) {
                    return Err(error.context(format!("recording the attempt also failed: {retry}")));
                }
                Err(error)
            }
            result => result,
        }
    }

    fn record_federation_executor_outcome_once<G>(
        &self, operation: &FederationExecutorOperation, outcome: Option<FederationExecutorOutcome>,
        now: chrono::DateTime<chrono::Utc>, authorize: impl FnOnce() -> Result<G>,
    ) -> Result<()> {
        let _writer = self.lock_writer("recording a remote wake outcome")?;
        let mut connection = self.connection()?;
        let transaction = begin_mailbox_write(&mut connection, "recording a remote wake outcome")?;
        // Settlement seeds the row; polling selects only seeded rows. This
        // keeps a direct record from failing on a row that is missing.
        seed_federation_executor_outcome(&transaction, operation, now)?;
        let existing = self.federation_executor_outcome_on(&transaction,
            &operation.central_host_id, &operation.operation_key)?
            .ok_or_else(|| anyhow!("remote wake outcome row is missing"))?;
        if existing.terminal { return Ok(()); }
        let attempts = existing.attempts.saturating_add(1);
        let next = federation_executor_outcome_time(now + chrono::Duration::from_std(
            federation_executor_outcome_backoff(attempts)).expect("bounded backoff"));
        let (last_outcome, last_observed_at) = match outcome {
            Some(outcome) => (Some(outcome.wire().to_owned()), Some(federation_executor_outcome_time(now))),
            None => (existing.last_outcome.map(|outcome| outcome.wire().to_owned()), existing.last_observed_at.clone()),
        };
        let terminal = outcome.is_some_and(FederationExecutorOutcome::terminal);
        transaction.execute(
            "UPDATE federation_executor_outcomes SET last_outcome=?3, last_observed_at=?4,
               terminal=?5, attempts=?6, next_poll_at=?7
             WHERE central_host_id=?1 AND operation_key=?2 AND terminal = 0",
            rusqlite::params![operation.central_host_id, operation.operation_key, last_outcome,
                last_observed_at, terminal as i64, attempts, next],
        )?;
        let _authority = if outcome.is_some() { Some(authorize()?) } else { None };
        transaction.commit()?;
        Ok(())
    }
}

impl AppState {
    /// One bounded pass of central's outcome polling, which the wake drain
    /// owner starts on its fallback tick in a task of its own, one at a time.
    async fn poll_federation_executor_outcomes(&self) {
        let deadline = std::time::Instant::now() + FEDERATION_EXECUTOR_OUTCOME_PASS_BUDGET;
        let (central, hosts) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Ok(central) = federation_mailbox_central_locked(&inner) else { return; };
            let hosts = inner.federation.placements.values()
                .map(|placement| placement.identity.host_id.clone())
                .filter(|host| *host != central.host_id)
                .collect::<BTreeSet<_>>();
            (central, hosts)
        };
        let reachable = hosts.into_iter().filter(|host| self.federation_peer_handle(host)
            .is_some_and(|peer| peer.channel.current_lease().is_ok())).collect::<Vec<_>>();
        let app = self.clone();
        let now = chrono::Utc::now();
        let candidates = match run_blocking_api(move || app.mailbox_store.federation_executor_outcome_candidates(
            &central.host_id, &reachable, now, FEDERATION_EXECUTOR_OUTCOME_POLL_BATCH).map_err(mailbox_api_error)).await
        {
            Ok(candidates) => candidates,
            Err(error) => { eprintln!("remote wake outcome scan unavailable ({})", error.status); return; }
        };
        for operation in candidates {
            if self.federation_executor_wake_signal.is_stopping() { return; }
            if std::time::Instant::now() >= deadline { return; }
            if let Err(error) = self.poll_federation_executor_outcome(operation).await {
                eprintln!("remote wake outcome remains pending ({})", error.status);
            }
        }
    }

    async fn poll_federation_executor_outcome(&self, operation: FederationExecutorOperation) -> Result<(), ApiError> {
        let peer = self.federation_peer_handle(&operation.target_host_id)
            .ok_or_else(federation_transport_unavailable)?;
        let lease = peer.channel.current_lease()?;
        #[cfg(test)]
        {
            // Single use: only the first poll after the hold is set waits.
            let hold = self.federation_executor_wake_signal.outcome_poll_hold.lock().unwrap().take();
            if let Some((entered, release)) = hold {
                let _ = entered.send(());
                release.notified().await;
            }
        }
        let request = FederationExecutorOutcomeRequest { protocol_version: FEDERATION_PROTOCOL_VERSION,
            payload_digest: operation.digest(), operation: operation.clone() };
        let answer: Result<FederationExecutorOutcomeReport, ApiError> = peer.rpc.request(
            FederationRpcRoute::ExecutorOutcome(lease.clone(), request)).await;
        // A mismatched, refused or unavailable answer is no evidence: the wake
        // stays pending and only its attempt is counted. Backoff bounds the logs.
        let outcome = match answer {
            Ok(report) if report.protocol_version == FEDERATION_PROTOCOL_VERSION
                && report.operation_key == operation.operation_key
                && report.payload_digest == operation.digest() => Some(report.outcome),
            Ok(_) => { eprintln!("remote wake outcome answer does not match its query"); None }
            Err(error) => { eprintln!("remote wake outcome answer unavailable ({})", error.status); None }
        };
        let app = self.clone();
        run_blocking_api(move || {
            let authority = FederationExecutorMirrorAuthority { app: &app, operation: &operation, lease: &lease };
            app.mailbox_store.record_federation_executor_outcome(&operation, outcome, chrono::Utc::now(),
                || authority.lock()).map_err(mailbox_api_error)
        }).await
    }
}

fn federation_executor_outcome_routes() -> Router<FederationListenerState> {
    Router::new().route(FEDERATION_EXECUTOR_OUTCOME_ROUTE, post(serve_federation_executor_outcome))
        .layer(DefaultBodyLimit::max(32 * 1024))
}

/// The satellite's read-only answer to its configured central about one
/// received wake. It changes no store and starts nothing.
async fn serve_federation_executor_outcome(
    axum::Extension(peer): axum::Extension<FederationAuthenticatedPeer>,
    Json(request): Json<FederationExecutorOutcomeRequest>,
) -> Result<Json<FederationExecutorOutcomeReport>, ApiError> {
    let lease = peer.lease.ok_or_else(federation_stale_link)?;
    let app = lease.channel.app.clone().ok_or_else(federation_transport_unavailable)?;
    let permit = peer.mailbox_permit.ok_or_else(federation_transport_unavailable)?;
    run_blocking_api(move || {
        // A cancelled or timed-out handler keeps its RPC slot until this
        // blocking read ends, so waiting reads stay within the peer's bound.
        let _permit = permit;
        #[cfg(test)]
        {
            let hold = app.federation_executor_wake_signal.outcome_answer_hold.lock().unwrap().take();
            if let Some((entered, release)) = hold {
                let _ = entered.send(());
                let _ = release.recv();
            }
        }
        let basis = lease.with_current(&app, |_, basis| Ok(basis.clone()))?;
        let operation = &request.operation;
        operation.validate().map_err(mailbox_api_error)?;
        {
            let inner = app.inner.lock().expect("state mutex poisoned");
            let local = inner.federation.local.as_ref().map(|local| local.host_id.clone());
            if request.protocol_version != FEDERATION_PROTOCOL_VERSION
                || basis.local_role != FederationRole::Satellite
                || basis.peer.host_id != operation.central_host_id
                || inner.preferences.federation.central.as_ref().map(|central| &central.host_id)
                    != Some(&operation.central_host_id)
                || local.as_deref() != Some(operation.target_host_id.as_str())
                || basis.enrollment_generation != operation.enrollment_generation
                || request.payload_digest != operation.digest()
            {
                return Err(federation_executor_conflict_api());
            }
        }
        let view = app.read_federation_executor_operation_view(&operation.central_host_id, &operation.operation_key)
            .map_err(mailbox_api_error)?
            .ok_or_else(|| ApiError::not_found("remote wake has no received journal entry"))?;
        if view.operation != *operation || !view.binding_current {
            return Err(federation_executor_conflict_api());
        }
        Ok(Json(FederationExecutorOutcomeReport { protocol_version: FEDERATION_PROTOCOL_VERSION,
            operation_key: operation.operation_key.clone(), payload_digest: operation.digest(),
            outcome: FederationExecutorOutcome::from_view(&view) }))
    }).await
}

fn federation_executor_conflict_api() -> ApiError {
    ApiError::conflict(FEDERATION_STALE_AUTHORITY)
}
