// Owns bounded process-local frozen pages and exact-boundary receipt issuance.
// A pending page grants no acknowledgement authority and survives no restart.
const FEDERATION_MAILBOX_FROZEN_KEYS: usize = 16;
const FEDERATION_MAILBOX_PAGE_BYTES: usize = 2 * 1024 * 1024;
const FEDERATION_MAILBOX_READ_TTL_MS: u64 = 10 * 60 * 1000;

#[cfg(test)]
thread_local! {
    static TEST_MAILBOX_PAGE_SNAPSHOT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

struct FederationMailboxReadCache {
    incarnation: String,
    offered: HashMap<String, std::time::Instant>,
    entries: HashMap<String, Arc<FederationMailboxFrozenRead>>,
}

impl Default for FederationMailboxReadCache {
    fn default() -> Self {
        Self { incarnation: Uuid::new_v4().to_string(), offered: HashMap::new(), entries: HashMap::new() }
    }
}

impl FederationMailboxReadCache {
    fn prune(&mut self) {
        let ttl = Duration::from_millis(FEDERATION_MAILBOX_READ_TTL_MS);
        self.offered.retain(|_, accepted| accepted.elapsed() < ttl);
        self.entries.retain(|_, entry| entry.accepted.elapsed() < ttl);
    }

    // An unconsumed offer grants only admission, never read or ACK authority.
    // Offers and pages share the same bounded ring; no tombstone journal grows.
    fn offer_key(&mut self) -> Option<String> {
        self.prune();
        if self.entries.len() + self.offered.len() >= FEDERATION_MAILBOX_FROZEN_KEYS {
            let reclaim = self.entries.iter().filter(|(_, entry)|
                Arc::strong_count(entry) == 1 && !entry.issuing.load(Ordering::Acquire)
                    && (entry.page.receipt_state.as_deref() != Some("receiptPending")
                        || entry.receipt.lock().expect("read receipt poisoned").is_some()))
                .min_by_key(|(_, entry)| entry.accepted).map(|(key, _)| key.clone());
            if let Some(key) = reclaim { self.entries.remove(&key); }
            else if let Some(key) = self.offered.iter().min_by_key(|(_, time)| **time)
                .map(|(key, _)| key.clone()) { self.offered.remove(&key); }
            else { return None; }
        }
        let key = federation_mailbox_new_read_key(&self.incarnation, federation_mailbox_read_now_ms());
        self.offered.insert(key.clone(), std::time::Instant::now());
        Some(key)
    }
}

struct FederationMailboxFrozenRead {
    accepted: std::time::Instant,
    scope: String,
    input: String,
    page: MailboxReadResponse,
    issuing: AtomicBool,
    receipt: Mutex<Option<String>>,
}

// The native API bridge keeps only its unresolved key and exact inputs, never
// a satellite copy of canonical messages. CLI processes share this host state.
struct FederationMailboxClientRead {
    binding: FederationMailboxBinding,
    input: String,
    key: Option<String>,
    accepted: std::time::Instant,
    in_flight: bool,
    owner: Uuid,
}

struct FederationMailboxReadFlight<'a> {
    intents: &'a Mutex<HashMap<String, FederationMailboxClientRead>>,
    slot: String,
    owner: Uuid,
}

impl Drop for FederationMailboxReadFlight<'_> {
    fn drop(&mut self) {
        let mut intents = self.intents.lock().expect("mailbox read intents poisoned");
        if let Some(intent) = intents.get_mut(&self.slot).filter(|intent| intent.owner == self.owner) {
            intent.in_flight = false;
            if intent.key.is_none() { intents.remove(&self.slot); }
        }
    }
}

fn federation_mailbox_read_now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default().as_millis().min(u64::MAX as u128) as u64
}

fn federation_mailbox_new_read_key(incarnation: &str, epoch: u64) -> String {
    format!("mr2:{incarnation}:{epoch}:{}", Uuid::new_v4())
}

fn federation_mailbox_read_expired() -> ApiError {
    ApiError::from_status(StatusCode::GONE,
        "readKeyExpired; the frozen page expired or the central process restarted; start a fresh logical read")
        .with_kind(ApiErrorKind::FederationMailboxReadExpired)
}

impl AppState {
    fn federation_mailbox_read_page(
        &self, authority: &FederationMailboxAuthority, mailbox: &str,
        after: Option<u64>, limit: u64, issue: bool, key: Option<String>, resolve: bool,
    ) -> Result<MailboxReadResponse, ApiError> {
        let limit = limit.clamp(1, FEDERATION_MAILBOX_PAGE_LIMIT);
        let scope = sha256_hex(&serde_json::to_vec(&(&authority.actor, mailbox))
            .expect("read scope serializes"));
        let input = sha256_hex(&serde_json::to_vec(&(&scope, after, limit, issue))
            .expect("read input serializes"));
        // This lock serializes selection and bounded read-only capture. It is
        // released before writer admission or issuance, including same-key calls.
        let entry = {
            let mut cache = self.mailbox_store.federation_reads.lock()
                .expect("mailbox read cache poisoned");
            cache.prune();
            let key = match key {
                Some(key) => key,
                None => cache.offer_key().ok_or_else(|| ApiError::from_status(StatusCode::TOO_MANY_REQUESTS,
                    "mailbox frozen-page capacity reached; resolve a retained pending page first"))?,
            };
            if key.len() > 160 { return Err(federation_mailbox_read_expired()); }
            let parts: Vec<_> = key.split(':').collect();
            let epoch = parts.get(2).and_then(|value| value.parse::<u64>().ok());
            let now = federation_mailbox_read_now_ms();
            if parts.len() != 4 || parts[0] != "mr2" || parts[1] != cache.incarnation
                || Uuid::parse_str(parts[3]).ok().is_none_or(|id| id.is_nil())
                || epoch.is_none_or(|epoch| now.saturating_sub(epoch) >= FEDERATION_MAILBOX_READ_TTL_MS
                    || epoch > now.saturating_add(2000)) {
                return Err(federation_mailbox_read_expired());
            }
            if let Some(entry) = cache.entries.get(&key) {
                if entry.input != input {
                    return Err(ApiError::conflict("readKey input conflict; preserve the identical original read")
                        .with_kind(ApiErrorKind::FederationMailboxReadConflict));
                }
                entry.clone()
            } else {
                // Only a server-issued, unconsumed offer may select new rows.
                // Replaying either flag after reclamation must expire, never
                // reconstruct a different page under a previously accepted key.
                if resolve { return Err(federation_mailbox_read_expired()); }
                let offered_at = cache.offered.remove(&key).ok_or_else(federation_mailbox_read_expired)?;
                // A repeated ordinary tool call may finish the one still-owed
                // identical page without asking the agent to manage a key.
                let pending = cache.entries.values().find(|entry| entry.scope == scope
                    && entry.page.next_after_sequence.is_some()
                    && entry.page.receipt_state.as_deref() == Some("receiptPending")
                    && entry.receipt.lock().expect("read receipt poisoned").is_none()).cloned();
                if let Some(entry) = pending {
                    if entry.input != input {
                        return Err(ApiError::from_status(StatusCode::TOO_MANY_REQUESTS,
                            "receiptPending; finish the retained read before changing its inputs"));
                    }
                    entry
                } else {
                    // Consuming the offer reserved this slot before capture.
                    let mut page = self.capture_federation_mailbox_page(authority, mailbox, after, limit,
                        Some((&key, &scope)))?;
                    page.read_key = Some(key.clone());
                    page.receipt_state = Some(if issue && page.next_after_sequence.is_some() {
                        "receiptPending" } else { "emptyOrPreview" }.to_owned());
                    page.cursor_generation = Some(sha256_hex(&serde_json::to_vec(
                        &(&scope, page.processed_through)).expect("cursor identity serializes")));
                    page.page_id = None;
                    page.page_id = Some(format!("mailbox-page-v2:{}", sha256_hex(&serde_json::to_vec(&(&input, &page))
                        .expect("page identity serializes"))));
                    if serde_json::to_vec(&page).map_err(|_| ApiError::internal("page encoding failed"))?.len()
                        > FEDERATION_MAILBOX_PAGE_BYTES {
                        return Err(ApiError::from_status(StatusCode::PAYLOAD_TOO_LARGE,
                            "mailbox page exceeds the negotiated byte bound"));
                    }
                    let entry = Arc::new(FederationMailboxFrozenRead {
                        accepted: offered_at, scope, input, page,
                        issuing: AtomicBool::new(false), receipt: Mutex::new(None),
                    });
                    cache.entries.insert(key, entry.clone());
                    entry
                }
            }
        };
        if issue && entry.page.next_after_sequence.is_some()
            && entry.receipt.lock().expect("read receipt poisoned").is_none()
            && entry.issuing.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            struct ReleaseIssuance<'a>(&'a AtomicBool);
            impl Drop for ReleaseIssuance<'_> {
                fn drop(&mut self) { self.0.store(false, Ordering::Release); }
            }
            let _release = ReleaseIssuance(&entry.issuing);
            if let Some(receipt) = self.issue_frozen_federation_mailbox_page(authority, mailbox, &entry.page)? {
                *entry.receipt.lock().expect("read receipt poisoned") = Some(receipt);
            }
        }
        // Issuance never substitutes current rows, cursor or wake metadata for
        // the frozen page. Revalidate live authority even on the cache fast path.
        let validate = || -> Result<()> {
            let mut connection = open_sqlite_state_read_connection(&self.mailbox_store.path)?;
            connection.busy_timeout(Duration::ZERO)?;
            let transaction = connection.transaction()?;
            require_mailbox_participant(&transaction, mailbox, &authority.actor.participant_key())?;
            let _current = authority.lock()?;
            transaction.commit()?;
            Ok(())
        };
        validate().map_err(mailbox_api_error)?;
        let mut page = entry.page.clone();
        page.receipt = entry.receipt.lock().expect("read receipt poisoned").clone();
        if page.receipt.is_some() { page.receipt_state = Some("issued".to_owned()); }
        Ok(page)
    }

    fn capture_federation_mailbox_page(
        &self, authority: &FederationMailboxAuthority, mailbox: &str,
        after: Option<u64>, limit: u64, descriptor: Option<(&str, &str)>,
    ) -> Result<MailboxReadResponse, ApiError> {
        let capture = || -> Result<MailboxReadResponse> {
            let mut connection = open_sqlite_state_read_connection(&self.mailbox_store.path)?;
            connection.busy_timeout(Duration::ZERO)?;
            let transaction = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
            let reader = authority.actor.participant_key();
            require_mailbox_participant(&transaction, mailbox, &reader)?;
            let processed = mailbox_processed_through(&transaction, mailbox, &reader)?;
            // Historical diagnostics own their scan cursor/window. Refreshing
            // the live cache from this snapshot could erase newer obligations.
            let mut reservations = FederationExecutorReservationCache {
                domain: Some(federation_executor_reservation_domain_locked(
                    &self.inner.lock().expect("state mutex poisoned"))
                    .map_err(|_| anyhow!("mailbox wake authority unavailable"))?),
                ..Default::default()
            };
            #[cfg(test)]
            TEST_MAILBOX_PAGE_SNAPSHOT.with(|hook| {
                let hook = hook.borrow_mut().take();
                if let Some(hook) = hook { hook(); }
            });
            let after = after.unwrap_or(processed);
            let mut page = MailboxReadResponse {
                receipt_state: None, read_key: None, page_id: None, cursor_generation: None,
                reader_session_id: Some(reader.clone()), messages: Vec::new(),
                after_sequence: after, processed_through: processed, receipt: None,
                has_more: false, next_after_sequence: None,
            };
            if let Some((key, scope)) = descriptor {
                page.read_key = Some(key.to_owned());
                page.cursor_generation = Some(sha256_hex(&serde_json::to_vec(&(scope, processed))?));
                page.page_id = Some(format!("mailbox-page-v2:{}", "0".repeat(64)));
                page.receipt_state = Some("receiptPending".to_owned());
            }
            let mut statement = transaction.prepare(
                "SELECT id,mailbox_id,sequence,sender_session_id,sender_name,target_session_id,
                        target_name,created_at,class,topic,state_stamp,body,idempotency_key,
                        unread_depth_at_append,notification_disposition
                 FROM mailbox_messages WHERE mailbox_id=?1 AND sequence>?2
                 AND NOT(target_session_id=?3 AND COALESCE(topic,'')=?4)
                 ORDER BY sequence LIMIT ?5")?;
            let mut rows = statement.query(rusqlite::params![mailbox, after, reader,
                DELEGATION_REVIEW_RESULT_TOPIC, limit + 1])?;
            let mut message_bytes = 0usize;
            while let Some(row) = rows.next()? {
                if page.messages.len() as u64 == limit { page.has_more = true; break; }
                let mut message = mailbox_message_from_row(row)?;
                let target_remote = {
                    let inner = self.inner.lock().expect("state mutex poisoned");
                    inner.federation.mailbox_directory.get(&message.target_session_id)
                        .map(|target| target.binding.identity.host_id != target.binding.central.host_id)
                };
                if target_remote == Some(true) {
                    message.notification_cause = Some(self.mailbox_store
                        .federation_executor_wake_state_on(&transaction, &message.id, &mut reservations)?);
                } else if target_remote == Some(false) {
                    let pending: bool = transaction.query_row(
                        "SELECT EXISTS(SELECT 1 FROM federation_mailbox_wake_obligations w
                         JOIN mailbox_messages m ON m.id=w.message_id
                         WHERE w.message_id=?1 AND m.dispatch_outcome IS NULL)",
                        [&message.id], |row| row.get(0))?;
                    if pending { message.notification_cause = Some("committed; native wake pending".into()); }
                }
                let size = serde_json::to_vec(&message)?.len();
                // Charge exact JSON message bytes and the larger, issued
                // descriptor. Do not serialize the growing message prefix on
                // every iteration or reserve an arbitrary per-page cushion.
                let envelope = MailboxReadResponse {
                    receipt_state: descriptor.map(|_| "issued".to_owned()),
                    read_key: page.read_key.clone(), page_id: page.page_id.clone(),
                    cursor_generation: page.cursor_generation.clone(),
                    reader_session_id: page.reader_session_id.clone(), messages: Vec::new(),
                    after_sequence: after, processed_through: processed,
                    receipt: descriptor.map(|_| "0".repeat(36)),
                    has_more: false, next_after_sequence: Some(message.sequence),
                };
                let bytes = serde_json::to_vec(&envelope)?.len() + message_bytes + size + page.messages.len();
                if bytes > FEDERATION_MAILBOX_PAGE_BYTES {
                    if page.messages.is_empty() { bail!("mailbox message cannot fit in a bounded page"); }
                    page.has_more = true; break;
                }
                message_bytes += size;
                page.next_after_sequence = Some(message.sequence);
                page.messages.push(message);
            }
            drop(rows); drop(statement);
            let _current = authority.lock()?;
            transaction.commit()?;
            Ok(page)
        };
        capture().map_err(mailbox_api_error)
    }

    fn issue_frozen_federation_mailbox_page(
        &self, authority: &FederationMailboxAuthority, mailbox: &str, page: &MailboxReadResponse,
    ) -> Result<Option<String>, ApiError> {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let Some(_writer) = lock_sqlite_state_writer_for(&self.mailbox_store.write_lock,
            self.mailbox_store.write_admission_timeout.min(Duration::from_secs(2))) else { return Ok(None); };
        let mut guard = match self.mailbox_store.connection.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(None),
            Err(std::sync::TryLockError::Poisoned(_)) => return Err(ApiError::internal("mailbox store unavailable")),
        };
        let connection = guard.as_mut().ok_or_else(|| ApiError::internal("mailbox store unavailable"))?;
        let old_timeout: u64 = connection.query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .map_err(|_| ApiError::internal("mailbox issuance unavailable"))?;
        connection.busy_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .map_err(|_| ApiError::internal("mailbox issuance unavailable"))?;
        let mut issue = || -> Result<String> {
            let transaction = begin_mailbox_write(connection, "recording frozen mailbox page issuance")?;
            let reader = authority.actor.participant_key();
            require_mailbox_participant(&transaction, mailbox, &reader)?;
            let through = page.next_after_sequence.expect("only nonempty pages are issued");
            let ids = {
                let mut statement = transaction.prepare(
                    "SELECT id FROM mailbox_messages WHERE mailbox_id=?1 AND sequence>?2 AND sequence<=?3
                     AND NOT(target_session_id=?4 AND COALESCE(topic,'')=?5) ORDER BY sequence")?;
                let rows = statement.query_map(rusqlite::params![mailbox, page.after_sequence, through,
                    reader, DELEGATION_REVIEW_RESULT_TOPIC], |row| row.get::<_, String>(0))?;
                rows.collect::<std::result::Result<Vec<_>, _>>()?
            };
            if ids != page.messages.iter().map(|message| message.id.clone()).collect::<Vec<_>>() {
                return Err(mailbox_store_error(MailboxStoreErrorKind::Conflict, "frozen mailbox page identity changed"));
            }
            transaction.execute(
                "INSERT INTO mailbox_read_pages(receipt,mailbox_id,session_id,after_sequence,through_sequence)
                 VALUES(?1,?2,?3,?4,?5) ON CONFLICT(mailbox_id,session_id,after_sequence,through_sequence) DO NOTHING",
                rusqlite::params![Uuid::new_v4().to_string(), mailbox, reader, page.after_sequence, through])?;
            let receipt = transaction.query_row(
                "SELECT receipt FROM mailbox_read_pages WHERE mailbox_id=?1 AND session_id=?2
                 AND after_sequence=?3 AND through_sequence=?4",
                rusqlite::params![mailbox, reader, page.after_sequence, through], |row| row.get(0))?;
            let _current = authority.lock()?;
            transaction.commit()?;
            Ok(receipt)
        };
        let result = issue();
        let restored = connection.busy_timeout(Duration::from_millis(old_timeout));
        if restored.is_err() { return Err(ApiError::internal("mailbox issuance configuration unavailable")); }
        match result {
            Ok(receipt) => Ok(Some(receipt)),
            Err(error) if error.downcast_ref::<MailboxStoreError>().is_some_and(|error|
                matches!(error.kind, MailboxStoreErrorKind::Conflict | MailboxStoreErrorKind::NotFound)) =>
                Err(mailbox_api_error(error)),
            // A failed/ambiguous commit grants no ACK authority. A later exact
            // readback may recover the already-issued receipt at this boundary.
            Err(_) => Ok(None),
        }
    }
}
