// Copies process-local observations before acquiring the SQLite read guard.
// Neither projection nor classification advances the producer's scan cache.
struct FederationJournalSnapshot {
    basis: String,
    local: FederationHostIdentity,
    role: FederationRole,
    scan: FederationJournalScan,
}

struct FederationJournalCandidate {
    rowid: i64,
    message: String,
    target: FederationMailboxBinding,
    payload_bytes: u64,
}

struct FederationJournalScan {
    summary: Option<Value>,
    domain: FederationExecutorReservationCache,
    scan_after: i64,
    prefix_complete: bool,
    candidates: Vec<FederationJournalCandidate>,
}

fn federation_journal_basis(
    state: &AppState,
) -> FederationJournalResult<(
    String,
    FederationHostIdentity,
    FederationRole,
    FederationExecutorReservationDomain,
)> {
    let inner = state.inner.lock().expect("state mutex poisoned");
    let local = inner
        .federation
        .local
        .clone()
        .ok_or(FederationJournalError::Unavailable)?;
    let domain = federation_executor_reservation_domain_locked(&inner)
        .map_err(|_| FederationJournalError::Unavailable)?;
    let role = inner.preferences.federation.role.clone();
    let bytes = serde_json::to_vec(&(
        &local,
        &inner.preferences.federation.central,
        &role,
        &domain.image,
    ))
    .map_err(|_| FederationJournalError::Storage)?;
    Ok((sha256_hex(&bytes), local, role, domain))
}

fn federation_journal_snapshot(
    state: &AppState,
) -> FederationJournalResult<FederationJournalSnapshot> {
    let (basis, local, role, domain) = federation_journal_basis(state)?;
    let cache = state
        .mailbox_store
        .executor_reservations
        .lock()
        .expect("executor reservation mutex poisoned");
    let observed = role == FederationRole::Central
        && cache.domain.as_ref() == Some(&domain)
        && (cache.scan_after != 0 || cache.exhausted || cache.scanned != 0);
    let requests = if observed {
        cache
            .window
            .iter()
            .map(|(rowid, (_, request))| (*rowid, request.clone()))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let summary = observed.then(|| cache.summary());
    let scan_after = cache.scan_after;
    let prefix_complete =
        cache.exhausted || cache.window.len() == FEDERATION_EXECUTOR_RESERVATION_WINDOW;
    drop(cache);
    // Serialization and validation run after releasing the producer mutex.
    let candidates = requests
        .into_iter()
        .map(|(rowid, request)| {
            let operation = request
                .operation(1)
                .map_err(|_| FederationJournalError::Storage)?;
            Ok(FederationJournalCandidate {
                rowid,
                message: request.wake.message_id,
                target: request.target,
                payload_bytes: operation.payload.len() as u64,
            })
        })
        .collect::<FederationJournalResult<Vec<_>>>()?;
    Ok(FederationJournalSnapshot {
        basis,
        local,
        role,
        scan: FederationJournalScan {
            summary,
            domain: FederationExecutorReservationCache {
                domain: observed.then_some(domain),
                ..Default::default()
            },
            scan_after,
            prefix_complete,
            candidates,
        },
    })
}

impl FederationJournalScan {
    fn project(
        &self,
        connection: &rusqlite::Connection,
        rows: Vec<FederationJournalWakeRow>,
    ) -> FederationJournalResult<Vec<(FederationJournalAnchor, Value)>> {
        let mut reserved = HashSet::new();
        let mut current_candidates = HashSet::new();
        if self.summary.is_some() && self.prefix_complete {
            // Recheck only metadata in the same read transaction as the page.
            // A cached rowid alone cannot identify a deleted/reused obligation.
            let ids: Vec<_> = self.candidates.iter().map(|row| row.rowid).collect();
            let encoded =
                serde_json::to_string(&ids).map_err(|_| FederationJournalError::Storage)?;
            let live = connection.prepare(
                "SELECT rowid,message_id,CASE WHEN length(CAST(target_binding_json AS BLOB))<=4096
                    THEN target_binding_json ELSE NULL END
                 FROM federation_mailbox_wake_obligations
                 WHERE rowid IN (SELECT value FROM json_each(?1)) AND state='pending'"
            ).map_err(|_| FederationJournalError::Storage)?
                .query_map([encoded], |row| Ok((row.get::<_, i64>(0)?,
                    (row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?))))
                .map_err(|_| FederationJournalError::Storage)?
                .collect::<rusqlite::Result<BTreeMap<_, _>>>()
                .map_err(|_| FederationJournalError::Storage)?;
            let candidates: Vec<_> = self
                .candidates
                .iter()
                .filter(|candidate| {
                    live.get(&candidate.rowid).is_some_and(|(message, target)| {
                        message == &candidate.message
                            && target
                                .as_deref()
                                .and_then(|text| {
                                    serde_json::from_str::<FederationMailboxBinding>(text).ok()
                                })
                                .as_ref()
                                == Some(&candidate.target)
                    })
                })
                .collect();
            let (count, bytes): (u64, u64) = connection.query_row(
                "SELECT COUNT(*),COALESCE(SUM(payload_bytes),0) FROM federation_executor_operations",
                [], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(|_| FederationJournalError::Storage)?;
            let prefix = federation_executor_reserved_prefix(
                count,
                bytes,
                candidates.iter().map(|candidate| candidate.payload_bytes),
            );
            reserved.extend(
                candidates
                    .iter()
                    .take(prefix)
                    .map(|candidate| candidate.rowid),
            );
            current_candidates.extend(candidates.iter().map(|candidate| candidate.rowid));
        }
        Ok(rows
            .into_iter()
            .map(|row| {
                let observed = self.summary.is_some() && row.anchor.rowid <= self.scan_after;
                let (classification, reason) = if row.state != "pending" || !observed {
                    ("unknown", None)
                } else if let Some(target) = &row.target {
                    if target.identity.host_id == target.central.host_id {
                        // The native scan skips central-local deliveries before
                        // checking remote authority or reserving remote work.
                        ("unknown", None)
                    } else if self.domain.stale(target) {
                        ("excluded", Some("stale_authority"))
                    } else if row.metadata_supported == Some(false) {
                        ("excluded", Some("unsupported"))
                    } else if reserved.contains(&row.anchor.rowid) {
                        ("reserved", None)
                    } else if current_candidates.contains(&row.anchor.rowid) {
                        ("excluded", Some("capacity"))
                    } else {
                        ("unknown", None)
                    }
                } else {
                    ("excluded", Some("unsupported"))
                };
                let value = json!({"messageId":row.message,"state":row.state,
                "targetBinding":row.target,"mirrorPresent":row.mirror,
                "classificationState":classification,"exclusionReason":reason});
                (row.anchor, value)
            })
            .collect())
    }
}
