// A process-local scan cache, never new durable authority. Storage retains
// every obligation and receipt; rebuilding this cache cannot grant a send.
const FEDERATION_EXECUTOR_RESERVATION_WINDOW: usize = 1000;
const FEDERATION_EXECUTOR_SCAN_PAGE: usize = 100;

#[derive(Clone, PartialEq, Eq)]
struct FederationExecutorReservationDomain {
    image: String,
    central: Option<FederationHostIdentity>,
    current: Vec<FederationMailboxBinding>,
}

#[derive(Default)]
struct FederationExecutorReservationCache {
    domain: Option<FederationExecutorReservationDomain>,
    floor: i64,
    scan_after: i64,
    exhausted: bool,
    scanned: usize,
    stale: u64,
    unsupported: u64,
    window: BTreeMap<i64, (String, FederationExecutorWakeRequest)>,
}

impl FederationExecutorReservationCache {
    // Crate-local read surface for the separately owned operator diagnostic.
    // It intentionally contains no payload or sender display metadata.
    #[cfg_attr(not(test), allow(dead_code))]
    fn summary(&self) -> Value {
        json!({"floorRowid":self.floor,"scannedLastPass":self.scanned,
            "windowOccupancy":self.window.len(),"excluded":{
                "staleAuthority":self.stale,"unsupported":self.unsupported}})
    }

    fn stale(&self, binding: &FederationMailboxBinding) -> bool {
        self.domain.as_ref().is_some_and(|domain|
            domain.central.as_ref() != Some(&binding.central) || !domain.current.contains(binding))
    }

    fn refresh(&mut self, connection: &rusqlite::Connection)
        -> Result<Vec<(String, FederationExecutorWakeRequest)>>
    {
        // Recheck cached state with one bounded indexed read, not one query
        // per message. A settled row frees its window position immediately.
        let rowids = serde_json::to_string(&self.window.keys().collect::<Vec<_>>())?;
        let pending = connection.prepare(
            "SELECT rowid FROM federation_mailbox_wake_obligations
             WHERE rowid IN (SELECT value FROM json_each(?1)) AND state='pending'"
        )?.query_map([rowids], |row| row.get::<_, i64>(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        self.window.retain(|id, _| pending.contains(id));
        self.scanned = 0;
        // At most ten pages per call, including permanently excluded rows.
        // scan_after carries the next page across calls; an all-invalid page
        // cannot cause an endless restart at row zero.
        while self.window.len() < FEDERATION_EXECUTOR_RESERVATION_WINDOW
            && self.scanned < FEDERATION_EXECUTOR_RESERVATION_WINDOW
        {
            let raw = connection.prepare(
                "SELECT w.rowid,w.state,w.target_binding_json,m.mailbox_id,m.id,m.sequence,
                 m.unread_depth_at_append,m.sender_session_id,m.sender_name,m.topic
                 FROM federation_mailbox_wake_obligations w JOIN mailbox_messages m ON m.id=w.message_id
                 WHERE w.rowid>?1 ORDER BY w.rowid LIMIT ?2"
            )?.query_map(rusqlite::params![self.scan_after, FEDERATION_EXECUTOR_SCAN_PAGE as u64], |row| Ok((
                row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
                FederationExecutorWake { mailbox_id: row.get(3)?, message_id: row.get(4)?,
                    sequence: row.get(5)?, unread_count: row.get(6)?, sender_session_id: row.get(7)?,
                    sender_name: row.get(8)?, topic: row.get(9)? },
            )))?.collect::<rusqlite::Result<Vec<_>>>()?;
            // A store/I/O error above advances nothing in this page. Only a
            // deterministic content refusal below is an exclusion.
            self.exhausted = raw.len() < FEDERATION_EXECUTOR_SCAN_PAGE;
            if raw.is_empty() { break; }
            for (id, state, binding, wake) in raw {
                if self.window.len() == FEDERATION_EXECUTOR_RESERVATION_WINDOW { break; }
                self.scan_after = id;
                self.scanned += 1;
                if state != "pending" { continue; }
                let target: FederationMailboxBinding = match serde_json::from_str(&binding) {
                    Ok(target) => target,
                    Err(_) => { self.unsupported += 1; continue; }
                };
                if target.identity.host_id == target.central.host_id { continue; }
                if self.stale(&target) { self.stale += 1; continue; }
                let request = FederationExecutorWakeRequest {
                    protocol_version: FEDERATION_PROTOCOL_VERSION,
                    operation_key: format!("mailbox-wake:{}", wake.message_id), target,
                    wake: wake.with_display_copies(),
                };
                if request.operation(1).is_err() { self.unsupported += 1; continue; }
                self.window.insert(id, (binding, request));
            }
            if self.exhausted { break; }
        }
        self.floor = self.window.first_key_value().map_or(self.scan_after, |(id, _)| id - 1);
        if !self.exhausted && self.window.len() < FEDERATION_EXECUTOR_RESERVATION_WINDOW {
            // Unknown older reservations may exist after restart. Finish the
            // prefix incrementally before new dispatch or receiver admission.
            return Err(mailbox_store_error(MailboxStoreErrorKind::Capacity,
                "remote wake reservation scan pending; retry without changing identity"));
        }
        let (count, bytes) = connection.query_row(
            "SELECT COUNT(*),COALESCE(SUM(payload_bytes),0) FROM federation_executor_operations",
            [], |row| Ok((row.get::<_, u64>(0)?, row.get::<_, u64>(1)?)),
        )?;
        let mut requests = self.window.values().cloned().collect::<Vec<_>>();
        let sizes = requests.iter().map(|(_, request)| request.operation(1)
            .expect("cached candidates validated").payload.len() as u64);
        let reserved = federation_executor_reserved_prefix(count, bytes, sizes);
        requests.truncate(reserved);
        Ok(requests)
    }
}

fn federation_executor_reservation_domain_locked(inner: &StateInner)
    -> Result<FederationExecutorReservationDomain, ApiError>
{
    if !inner.federation_durable || inner.federation_continuity_lost {
        return Err(federation_transport_unavailable());
    }
    // Link incarnations, endpoint verification and enabled/settings
    // edits are wait conditions, never permanent exclusions.
    let central = inner.federation.local.clone();
    let mut current = Vec::new();
    let mut identities = BTreeMap::new();
    for (key, placement) in &inner.federation.placements {
        let peer = inner.federation.peers.get(&placement.identity.host_id);
        let valid = placement.state != FederationPlacementState::Completed
            && central.as_ref() == Some(&placement.peer.local)
            && peer.is_some_and(|peer|
                peer.identity == placement.peer.peer
                    && peer.enrollment_generation == placement.peer.enrollment_generation
                    && peer.membership != FederationMembershipState::Revoked);
        let binding = FederationMailboxBinding { central: placement.peer.local.clone(),
            identity: placement.identity.clone(), placement_generation: placement.placement_generation };
        identities.insert(key.clone(), (binding.clone(), placement.peer.enrollment_generation, valid));
        if valid { current.push(binding); }
    }
    let image = serde_json::to_string(&(&central, &identities, &inner.preferences.federation.role))
        .expect("durable reservation identities serialize");
    Ok(FederationExecutorReservationDomain { image, central, current })
}

impl AppState {
    fn refresh_federation_executor_reservation_domain(&self) -> Result<(), ApiError> {
        let domain = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            federation_executor_reservation_domain_locked(&inner)?
        };
        let mut cache = self.mailbox_store.executor_reservations.lock()
            .expect("executor reservation mutex poisoned");
        if cache.domain.as_ref() != Some(&domain) {
            *cache = FederationExecutorReservationCache { domain: Some(domain), ..Default::default() };
        }
        Ok(())
    }
}
