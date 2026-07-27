/*
Repository-scoped coordination board persistence.

The durable mailbox is an edge-triggered activation channel. This store owns
the complementary level-triggered state: small, versioned JSON facts that root
sessions in one local project can inspect without replaying conversation.
Writes never wake a session. HTTP/MCP authorization and project-membership
checks live in the integration layer; this module owns validation, CAS,
idempotency, persistence, history retention, and snapshot consistency.
*/

const COORDINATION_BOARD_WRITER_ADMISSION_TIMEOUT: Duration = Duration::from_secs(5);
const COORDINATION_BOARD_MAX_KEY_BYTES: usize = 128;
const COORDINATION_BOARD_MAX_VALUE_BYTES: usize = 4 * 1024;
const COORDINATION_BOARD_MAX_VALUE_DEPTH: usize = 32;
const COORDINATION_BOARD_MAX_IDEMPOTENCY_KEY_BYTES: usize = 256;
const COORDINATION_BOARD_MAX_STATE_STAMP_BYTES: usize = 256;
const COORDINATION_BOARD_MAX_SCOPE_ID_BYTES: usize = 256;
const COORDINATION_BOARD_MAX_AUTHOR_ID_BYTES: usize = 256;
const COORDINATION_BOARD_MAX_AUTHOR_NAME_BYTES: usize = 256;
const COORDINATION_BOARD_DEFAULT_PAGE_SIZE: usize = 100;
const COORDINATION_BOARD_MAX_PAGE_SIZE: usize = 200;
const COORDINATION_BOARD_MAX_LIVE_ENTRIES_PER_SCOPE: usize = 512;
const COORDINATION_BOARD_MAX_DISTINCT_KEYS_PER_SCOPE: usize = 4_096;
const COORDINATION_BOARD_HISTORY_REVISIONS_PER_KEY: usize = 100;
const COORDINATION_BOARD_IDEMPOTENCY_RECEIPTS_PER_SCOPE: usize = 4_096;
const COORDINATION_BOARD_LIFECYCLE_CLEANUP_TIMEOUT: Duration =
    Duration::from_millis(100);
const COORDINATION_BOARD_READ_SAFE_RETRY_CLAUSE: &str =
    "no mutation was attempted by this read operation, so retry the same request";
const COORDINATION_BOARD_WRITE_SAFE_RETRY_CLAUSE: &str =
    "no coordination board write was committed by this operation, so retry the same request";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CoordinationBoardStoreErrorKind {
    Validation,
    Conflict,
    NotFound,
    Retryable,
    Disabled,
}

#[derive(Clone, Debug)]
struct CoordinationBoardStoreError {
    kind: CoordinationBoardStoreErrorKind,
    message: String,
    current: Option<CoordinationBoardHead>,
    current_generation: Option<u64>,
}

impl std::fmt::Display for CoordinationBoardStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CoordinationBoardStoreError {}

fn coordination_board_store_error(
    kind: CoordinationBoardStoreErrorKind,
    message: impl Into<String>,
) -> anyhow::Error {
    CoordinationBoardStoreError {
        kind,
        message: message.into(),
        current: None,
        current_generation: None,
    }
    .into()
}

fn coordination_board_conflict(
    message: impl Into<String>,
    current: Option<CoordinationBoardHead>,
    current_generation: Option<u64>,
) -> anyhow::Error {
    CoordinationBoardStoreError {
        kind: CoordinationBoardStoreErrorKind::Conflict,
        message: message.into(),
        current,
        current_generation,
    }
    .into()
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CoordinationBoardHead {
    key: String,
    revision: u64,
    /// Scope generation at which this key was last written. This is not the
    /// scope's current generation unless this key was the most recent write.
    updated_at_generation: u64,
    value: Value,
    deleted: bool,
    author_session_id: String,
    author_name: String,
    updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    state_stamp: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CoordinationBoardGetResponse {
    #[serde(flatten)]
    head: CoordinationBoardHead,
    /// Current generation of the whole scope at the same read snapshot.
    scope_generation: u64,
}

impl std::ops::Deref for CoordinationBoardGetResponse {
    type Target = CoordinationBoardHead;

    fn deref(&self) -> &Self::Target {
        &self.head
    }
}

impl CoordinationBoardHead {
    fn is_deleted(&self) -> bool {
        self.deleted
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CoordinationBoardSetReceipt {
    key: String,
    revision: u64,
    prior_revision: u64,
    generation: u64,
    value: Value,
    deleted: bool,
    author_session_id: String,
    author_name: String,
    updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    state_stamp: Option<String>,
    duplicate: bool,
}

#[derive(Clone, Debug)]
struct CoordinationBoardSetInput {
    scope_project_id: String,
    key: String,
    value: Option<Value>,
    expected_revision: u64,
    author_session_id: String,
    author_name: String,
    idempotency_key: String,
    state_stamp: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct CoordinationBoardListRequest {
    scope_project_id: String,
    after_key: Option<String>,
    limit: Option<usize>,
    snapshot_generation: Option<u64>,
    known_generation: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CoordinationBoardListPage {
    generation: u64,
    entries: Vec<CoordinationBoardHead>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_after_key: Option<String>,
    unchanged: bool,
}

struct CoordinationBoardStore {
    connection: Mutex<Option<rusqlite::Connection>>,
    interrupt_handle: Option<rusqlite::InterruptHandle>,
    write_lock: Arc<SqliteStateWriterAdmission>,
    write_admission_timeout: Duration,
    #[cfg(test)]
    write_ordering_hook: Option<CoordinationBoardWriteOrderingHook>,
}

#[cfg(test)]
#[derive(Clone)]
struct CoordinationBoardWriteOrderingHook {
    connection_acquired: Arc<std::sync::Barrier>,
    continue_to_writer_admission: Arc<std::sync::Barrier>,
}

struct CoordinationBoardConnectionGuard<'a> {
    guard: std::sync::MutexGuard<'a, Option<rusqlite::Connection>>,
}

impl std::ops::Deref for CoordinationBoardConnectionGuard<'_> {
    type Target = rusqlite::Connection;

    fn deref(&self) -> &Self::Target {
        self.guard
            .as_ref()
            .expect("enabled coordination board store should own a connection")
    }
}

impl CoordinationBoardStore {
    fn open(path: &FsPath) -> Result<Self> {
        Self::open_with_write_admission_timeout(path, COORDINATION_BOARD_WRITER_ADMISSION_TIMEOUT)
    }

    fn open_with_write_admission_timeout(
        path: &FsPath,
        write_admission_timeout: Duration,
    ) -> Result<Self> {
        let connection = open_sqlite_state_connection(path)?;
        ensure_sqlite_coordination_schema_for_path(&connection, path)?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .with_context(|| {
                format!(
                    "failed to enable coordination-board foreign keys for `{}`",
                    path.display()
                )
            })?;
        let interrupt_handle = connection.get_interrupt_handle();
        Ok(Self {
            connection: Mutex::new(Some(connection)),
            interrupt_handle: Some(interrupt_handle),
            write_lock: sqlite_state_write_lock(path),
            write_admission_timeout,
            #[cfg(test)]
            write_ordering_hook: None,
        })
    }

    #[cfg(test)]
    fn disabled_for_tests() -> Self {
        Self {
            connection: Mutex::new(None),
            interrupt_handle: None,
            write_lock: Arc::new(SqliteStateWriterAdmission::default()),
            write_admission_timeout: COORDINATION_BOARD_WRITER_ADMISSION_TIMEOUT,
            write_ordering_hook: None,
        }
    }

    fn interrupt_current_operation(&self) {
        if let Some(interrupt_handle) = &self.interrupt_handle {
            interrupt_handle.interrupt();
        }
    }

    fn connection(&self) -> Result<CoordinationBoardConnectionGuard<'_>> {
        self.connection_for_operation_with_timeout(
            "waiting for the coordination board connection",
            self.write_admission_timeout,
            COORDINATION_BOARD_READ_SAFE_RETRY_CLAUSE,
        )
    }

    fn connection_for_operation(
        &self,
        operation: &str,
    ) -> Result<CoordinationBoardConnectionGuard<'_>> {
        self.connection_for_operation_with_timeout(
            operation,
            self.write_admission_timeout,
            COORDINATION_BOARD_WRITE_SAFE_RETRY_CLAUSE,
        )
    }

    fn connection_for_operation_with_timeout(
        &self,
        operation: &str,
        timeout: Duration,
        safe_retry_clause: &str,
    ) -> Result<CoordinationBoardConnectionGuard<'_>> {
        let deadline = std::time::Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(std::time::Instant::now);
        loop {
            match self.connection.try_lock() {
                Ok(guard) => {
                    if guard.is_none() {
                        return Err(coordination_board_store_error(
                            CoordinationBoardStoreErrorKind::Disabled,
                            "coordination board storage is disabled for this AppState",
                        ));
                    }
                    return Ok(CoordinationBoardConnectionGuard { guard });
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    panic!("coordination board connection mutex poisoned")
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    let now = std::time::Instant::now();
                    if now >= deadline {
                        return Err(coordination_board_store_error(
                            CoordinationBoardStoreErrorKind::Retryable,
                            format!(
                                "coordination board storage is temporarily busy while {operation}; \
                                 {safe_retry_clause}"
                            ),
                        ));
                    }
                    std::thread::sleep(
                        deadline
                            .saturating_duration_since(now)
                            .min(Duration::from_millis(5)),
                    );
                }
            }
        }
    }

    fn lock_writer(&self, operation: &str) -> Result<SqliteStateWriterGuard<'_>> {
        self.lock_writer_with_timeout(operation, self.write_admission_timeout)
    }

    fn lock_writer_with_timeout(
        &self,
        operation: &str,
        timeout: Duration,
    ) -> Result<SqliteStateWriterGuard<'_>> {
        lock_sqlite_state_writer_for(&self.write_lock, timeout).ok_or_else(|| {
            coordination_board_store_error(
                CoordinationBoardStoreErrorKind::Retryable,
                format!(
                    "coordination board storage is temporarily busy while {operation}; \
                     {COORDINATION_BOARD_WRITE_SAFE_RETRY_CLAUSE}"
                ),
            )
        })
    }

    fn get(&self, scope_project_id: &str, key: &str) -> Result<CoordinationBoardGetResponse> {
        validate_coordination_board_scope_id(scope_project_id)?;
        validate_coordination_board_key(key)?;
        let connection = self.connection()?;
        let transaction = rusqlite::Transaction::new_unchecked(
            &connection,
            rusqlite::TransactionBehavior::Deferred,
        )
        .context("failed to begin coordination board key read snapshot")?;
        ensure_coordination_board_scope_active(&transaction, scope_project_id)?;
        let scope_generation =
            query_coordination_board_generation(&transaction, scope_project_id)?;
        let current = query_coordination_board_head(&transaction, scope_project_id, key)?;
        let result = match current {
            Some(head) if !head.is_deleted() => Ok(CoordinationBoardGetResponse {
                head,
                scope_generation,
            }),
            current => Err(CoordinationBoardStoreError {
                kind: CoordinationBoardStoreErrorKind::NotFound,
                message: format!(
                    "coordination board key `{key}` was not found in scope `{scope_project_id}`"
                ),
                current,
                current_generation: Some(scope_generation),
            }
            .into()),
        };
        // This is a read-only DEFERRED snapshot. Dropping it cannot replace
        // the authoritative head/NotFound outcome with a bookkeeping error;
        // there is nothing to commit.
        drop(transaction);
        result
    }

    fn list(&self, request: &CoordinationBoardListRequest) -> Result<CoordinationBoardListPage> {
        validate_coordination_board_scope_id(&request.scope_project_id)?;
        if let Some(after_key) = request.after_key.as_deref() {
            validate_coordination_board_key(after_key)?;
            if request.snapshot_generation.is_none() {
                return Err(coordination_board_store_error(
                    CoordinationBoardStoreErrorKind::Validation,
                    "coordination board pagination requires `snapshotGeneration` after the first page",
                ));
            }
        }
        if request.after_key.is_some() && request.known_generation.is_some() {
            return Err(coordination_board_store_error(
                CoordinationBoardStoreErrorKind::Validation,
                "`knownGeneration` is valid only for the first coordination board page",
            ));
        }
        let limit = request
            .limit
            .unwrap_or(COORDINATION_BOARD_DEFAULT_PAGE_SIZE);
        if !(1..=COORDINATION_BOARD_MAX_PAGE_SIZE).contains(&limit) {
            return Err(coordination_board_store_error(
                CoordinationBoardStoreErrorKind::Validation,
                format!(
                    "coordination board page limit must be between 1 and {}",
                    COORDINATION_BOARD_MAX_PAGE_SIZE
                ),
            ));
        }

        let connection = self.connection()?;
        let transaction = rusqlite::Transaction::new_unchecked(
            &connection,
            rusqlite::TransactionBehavior::Deferred,
        )
        .context("failed to begin coordination board read snapshot")?;
        ensure_coordination_board_scope_active(&transaction, &request.scope_project_id)?;
        let generation =
            query_coordination_board_generation(&transaction, &request.scope_project_id)?;

        if let Some(snapshot_generation) = request.snapshot_generation {
            if snapshot_generation != generation {
                return Err(coordination_board_conflict(
                    format!(
                        "coordination board scope changed during pagination: expected generation \
                         {snapshot_generation}, current generation is {generation}; restart the \
                         listing"
                    ),
                    None,
                    Some(generation),
                ));
            }
        }
        if request.after_key.is_none() && request.known_generation == Some(generation) {
            // This is a read-only DEFERRED snapshot. As in `get`, Drop is the
            // correct close operation: there is no write to commit and no
            // bookkeeping failure should replace the authoritative result.
            drop(transaction);
            return Ok(CoordinationBoardListPage {
                generation,
                entries: Vec::new(),
                next_after_key: None,
                unchanged: true,
            });
        }

        let query_limit = i64::try_from(limit + 1)
            .expect("coordination board page limit is bounded below i64::MAX");
        let mut entries = if let Some(after_key) = request.after_key.as_deref() {
            query_coordination_board_entries_after(
                &transaction,
                &request.scope_project_id,
                after_key,
                query_limit,
            )?
        } else {
            query_coordination_board_entries(&transaction, &request.scope_project_id, query_limit)?
        };
        let next_after_key = if entries.len() > limit {
            entries.truncate(limit);
            entries.last().map(|entry| entry.key.clone())
        } else {
            None
        };
        drop(transaction);
        Ok(CoordinationBoardListPage {
            generation,
            entries,
            next_after_key,
            unchanged: false,
        })
    }

    fn set(&self, input: &CoordinationBoardSetInput) -> Result<CoordinationBoardSetReceipt> {
        let canonical_value = validate_coordination_board_set_input(input)?;
        let request_hash = coordination_board_request_hash(input, canonical_value.as_deref());
        let canonical_receipt_value = canonical_value
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .context("failed to decode canonical coordination board receipt value")?
            .unwrap_or(Value::Null);
        let connection =
            self.connection_for_operation("waiting for the coordination board connection")?;
        #[cfg(test)]
        if let Some(hook) = self.write_ordering_hook.as_ref() {
            hook.connection_acquired.wait();
            hook.continue_to_writer_admission.wait();
        }
        // Acquire the private board connection before the writer admission
        // shared with mailboxes. A board reader may hold this connection for
        // its snapshot; waiting for it must not reserve the shared writer
        // queue and transitively delay unrelated mailbox writes.
        let _write_guard = self.lock_writer("beginning coordination board update")?;
        let transaction =
            begin_coordination_board_write(&connection, "beginning coordination board update")?;
        ensure_coordination_board_scope_active(&transaction, &input.scope_project_id)?;

        if let Some((stored_hash, receipt_json)) = query_coordination_board_idempotency(
            &transaction,
            &input.scope_project_id,
            &input.author_session_id,
            &input.idempotency_key,
        )? {
            if stored_hash != request_hash {
                let current_generation =
                    query_coordination_board_generation(&transaction, &input.scope_project_id)?;
                return Err(coordination_board_conflict(
                    format!(
                        "coordination board idempotency key `{}` was already used for a different \
                         update",
                        input.idempotency_key
                    ),
                    None,
                    Some(current_generation),
                ));
            }
            let mut receipt: CoordinationBoardSetReceipt = serde_json::from_str(&receipt_json)
                .context("failed to decode stored coordination board receipt")?;
            receipt.duplicate = true;
            return Ok(receipt);
        }

        transaction
            .execute(
                "INSERT INTO coordination_board_scopes(scope_id, generation)
                 VALUES(?1, 0)
                 ON CONFLICT(scope_id) DO NOTHING",
                rusqlite::params![input.scope_project_id],
            )
            .context("failed to initialize coordination board scope")?;
        let current =
            query_coordination_board_head(&transaction, &input.scope_project_id, &input.key)?;
        let current_revision = current.as_ref().map_or(0, |head| head.revision);
        if current_revision != input.expected_revision {
            if current.is_none() && input.expected_revision > 0 {
                return Err(CoordinationBoardStoreError {
                    kind: CoordinationBoardStoreErrorKind::NotFound,
                    message: format!(
                        "coordination board key `{}` was not found in scope `{}`",
                        input.key, input.scope_project_id
                    ),
                    current: None,
                    current_generation: Some(query_coordination_board_generation(
                        &transaction,
                        &input.scope_project_id,
                    )?),
                }
                .into());
            }
            return Err(coordination_board_conflict(
                format!(
                    "coordination board revision conflict for `{}`: expected {}, current revision \
                     is {}",
                    input.key, input.expected_revision, current_revision
                ),
                current,
                Some(query_coordination_board_generation(
                    &transaction,
                    &input.scope_project_id,
                )?),
            ));
        }
        if input.value.is_none()
            && current
                .as_ref()
                .is_none_or(CoordinationBoardHead::is_deleted)
        {
            return Err(CoordinationBoardStoreError {
                kind: CoordinationBoardStoreErrorKind::NotFound,
                message: format!(
                    "coordination board key `{}` is already absent from scope `{}`",
                    input.key, input.scope_project_id
                ),
                current,
                current_generation: Some(query_coordination_board_generation(
                    &transaction,
                    &input.scope_project_id,
                )?),
            }
            .into());
        }
        let creates_live_entry = canonical_value.is_some()
            && current
                .as_ref()
                .is_none_or(CoordinationBoardHead::is_deleted);
        if canonical_value.is_some() && current.is_none() {
            let distinct_key_count =
                query_coordination_board_distinct_key_count(&transaction, &input.scope_project_id)?;
            if distinct_key_count >= COORDINATION_BOARD_MAX_DISTINCT_KEYS_PER_SCOPE as u64 {
                return Err(coordination_board_store_error(
                    CoordinationBoardStoreErrorKind::Validation,
                    format!(
                        "coordination board scope `{}` has reached the {}-distinct-key lifetime \
                         limit; reuse a retained tombstone or delete the project before \
                         introducing another key",
                        input.scope_project_id, COORDINATION_BOARD_MAX_DISTINCT_KEYS_PER_SCOPE
                    ),
                ));
            }
        }
        if creates_live_entry {
            let live_entry_count =
                query_coordination_board_live_entry_count(&transaction, &input.scope_project_id)?;
            if live_entry_count >= COORDINATION_BOARD_MAX_LIVE_ENTRIES_PER_SCOPE as u64 {
                return Err(coordination_board_store_error(
                    CoordinationBoardStoreErrorKind::Validation,
                    format!(
                        "coordination board scope `{}` has reached the {}-live-key limit; delete \
                         or reuse a live key before creating or restoring another",
                        input.scope_project_id, COORDINATION_BOARD_MAX_LIVE_ENTRIES_PER_SCOPE
                    ),
                ));
            }
        }

        if canonical_value.is_some()
            && let Some(current) = current.as_ref()
        {
            insert_coordination_board_history(&transaction, &input.scope_project_id, current)?;
        }
        let revision = current_revision
            .checked_add(1)
            .ok_or_else(|| anyhow!("coordination board revision space exhausted"))?;
        let prior_generation =
            query_coordination_board_generation(&transaction, &input.scope_project_id)?;
        let generation = prior_generation
            .checked_add(1)
            .ok_or_else(|| anyhow!("coordination board generation space exhausted"))?;
        let revision_sql =
            i64::try_from(revision).context("coordination board revision exceeds SQLite range")?;
        let generation_sql = i64::try_from(generation)
            .context("coordination board generation exceeds SQLite range")?;
        let updated_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

        transaction
            .execute(
                "UPDATE coordination_board_scopes
                 SET generation = ?2
                 WHERE scope_id = ?1",
                rusqlite::params![input.scope_project_id, generation_sql],
            )
            .context("failed to advance coordination board scope generation")?;
        transaction
            .execute(
                "INSERT INTO coordination_board_entries(
                   scope_id, key, revision, generation, value_json,
                   author_session_id, author_name, updated_at, state_stamp
                 )
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(scope_id, key) DO UPDATE SET
                   revision = excluded.revision,
                   generation = excluded.generation,
                   value_json = excluded.value_json,
                   author_session_id = excluded.author_session_id,
                   author_name = excluded.author_name,
                   updated_at = excluded.updated_at,
                   state_stamp = excluded.state_stamp",
                rusqlite::params![
                    input.scope_project_id,
                    input.key,
                    revision_sql,
                    generation_sql,
                    canonical_value.as_deref(),
                    input.author_session_id,
                    input.author_name,
                    updated_at,
                    input.state_stamp,
                ],
            )
            .context("failed to persist coordination board head")?;
        if canonical_value.is_some() {
            trim_coordination_board_history(&transaction, &input.scope_project_id, &input.key)?;
        } else {
            purge_coordination_board_history(&transaction, &input.scope_project_id, &input.key)?;
        }

        let receipt = CoordinationBoardSetReceipt {
            key: input.key.clone(),
            revision,
            prior_revision: current_revision,
            generation,
            value: canonical_receipt_value,
            deleted: canonical_value.is_none(),
            author_session_id: input.author_session_id.clone(),
            author_name: input.author_name.clone(),
            updated_at: updated_at.clone(),
            state_stamp: input.state_stamp.clone(),
            duplicate: false,
        };
        let receipt_json = serde_json::to_string(&receipt)
            .context("failed to encode coordination board receipt")?;
        transaction
            .execute(
                "INSERT INTO coordination_board_idempotency(
                   scope_id, author_session_id, idempotency_key,
                   request_hash, receipt_json, created_at
                 )
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    input.scope_project_id,
                    input.author_session_id,
                    input.idempotency_key,
                    request_hash,
                    receipt_json,
                    updated_at,
                ],
            )
            .context("failed to persist coordination board idempotency receipt")?;
        trim_coordination_board_idempotency(&transaction, &input.scope_project_id)?;
        // SQLite leaves the transaction active when COMMIT returns
        // BUSY/LOCKED; dropping it rolls back. The shared mapper can therefore
        // truthfully retain the structural no-commit clause.
        transaction.commit().map_err(|err| {
            coordination_board_sqlite_write_error("committing coordination board update", err)
        })?;
        Ok(receipt)
    }

    #[cfg(test)]
    fn delete_scope(&self, scope_project_id: &str) -> Result<bool> {
        self.delete_scope_with_timeout(scope_project_id, self.write_admission_timeout)
    }

    fn delete_scope_with_timeout(
        &self,
        scope_project_id: &str,
        timeout: Duration,
    ) -> Result<bool> {
        validate_coordination_board_scope_id(scope_project_id)?;
        let deadline = std::time::Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(std::time::Instant::now);
        let connection = self.connection_for_operation_with_timeout(
            "waiting to delete the coordination board scope",
            deadline.saturating_duration_since(std::time::Instant::now()),
            COORDINATION_BOARD_WRITE_SAFE_RETRY_CLAUSE,
        )?;
        let _write_guard = self.lock_writer_with_timeout(
            "deleting coordination board scope",
            deadline.saturating_duration_since(std::time::Instant::now()),
        )?;

        // The lifecycle budget is end-to-end, not just admission. Temporarily
        // shrink this long-lived connection's SQLite busy timeout to the
        // remaining budget before BEGIN and again before COMMIT; otherwise the
        // connection-wide five-second default could stall the persist worker
        // long after the 100ms cleanup deadline. The private connection guard
        // prevents another board operation from observing the temporary value.
        with_coordination_board_busy_timeout(
            &connection,
            deadline.saturating_duration_since(std::time::Instant::now()),
            || {
                let transaction = begin_coordination_board_write(
                    &connection,
                    "beginning coordination board scope deletion",
                )?;
                transaction
                    .execute(
                        "INSERT INTO coordination_board_deleted_scopes(scope_id, deleted_at)
                         VALUES(?1, ?2)
                         ON CONFLICT(scope_id) DO NOTHING",
                        rusqlite::params![
                            scope_project_id,
                            chrono::Utc::now()
                                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                        ],
                    )
                    .context("failed to fence deleted coordination board scope")?;
                let deleted = transaction
                    .execute(
                        "DELETE FROM coordination_board_scopes WHERE scope_id = ?1",
                        rusqlite::params![scope_project_id],
                    )
                    .context("failed to delete coordination board scope")?
                    > 0;
                transaction
                    .busy_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                    .context("failed to update coordination board lifecycle commit timeout")?;
                // As above, BUSY/LOCKED leaves the transaction active and Drop
                // rolls it back, so retrying the same request is safe.
                transaction.commit().map_err(|err| {
                    coordination_board_sqlite_write_error(
                        "committing coordination board scope deletion",
                        err,
                    )
                })?;
                Ok(deleted)
            },
        )
    }

    fn delete_scope_for_project_lifecycle(&self, scope_project_id: &str) -> Result<bool> {
        // Cleanup is backed by a durable outbox and retried by its dedicated
        // worker after later persist ticks and boots. It therefore uses a
        // short admission budget so a busy secondary coordination database
        // releases that worker quickly for backoff or shutdown.
        match self.delete_scope_with_timeout(
            scope_project_id,
            COORDINATION_BOARD_LIFECYCLE_CLEANUP_TIMEOUT,
        ) {
            Err(err)
                if err
                    .downcast_ref::<CoordinationBoardStoreError>()
                    .is_some_and(|store_error| {
                        matches!(store_error.kind, CoordinationBoardStoreErrorKind::Disabled)
                    }) =>
            {
                Ok(false)
            }
            result => result,
        }
    }
}

fn ensure_coordination_board_scope_active(
    connection: &rusqlite::Connection,
    scope_project_id: &str,
) -> Result<()> {
    let deleted = connection
        .query_row(
            "SELECT EXISTS(
               SELECT 1
               FROM coordination_board_deleted_scopes
               WHERE scope_id = ?1
             )",
            rusqlite::params![scope_project_id],
            |row| row.get::<_, bool>(0),
        )
        .context("failed to inspect coordination board deletion fence")?;
    if deleted {
        return Err(CoordinationBoardStoreError {
            kind: CoordinationBoardStoreErrorKind::NotFound,
            message: format!(
                "coordination board scope `{scope_project_id}` was deleted with its project"
            ),
            current: None,
            current_generation: Some(0),
        }
        .into());
    }
    Ok(())
}

fn begin_coordination_board_write<'connection>(
    connection: &'connection rusqlite::Connection,
    operation: &str,
) -> Result<rusqlite::Transaction<'connection>> {
    rusqlite::Transaction::new_unchecked(connection, rusqlite::TransactionBehavior::Immediate)
        .map_err(|err| coordination_board_sqlite_write_error(operation, err))
}

fn coordination_board_sqlite_write_error(operation: &str, err: rusqlite::Error) -> anyhow::Error {
    if matches!(
        err.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    ) {
        return coordination_board_store_error(
            CoordinationBoardStoreErrorKind::Retryable,
            format!(
                "coordination board storage is temporarily busy while {operation}; \
                 {COORDINATION_BOARD_WRITE_SAFE_RETRY_CLAUSE}"
            ),
        );
    }
    anyhow!(err).context(format!("failed while {operation}"))
}

fn with_coordination_board_busy_timeout<T>(
    connection: &rusqlite::Connection,
    timeout: Duration,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    connection
        .busy_timeout(timeout)
        .context("failed to set coordination board lifecycle SQLite busy timeout")?;
    let operation_result = operation();
    let restore_result = connection
        .busy_timeout(SQLITE_BUSY_TIMEOUT)
        .context("failed to restore coordination board SQLite busy timeout");
    match (operation_result, restore_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(err), Ok(())) => Err(err),
        (Ok(_), Err(err)) => Err(err),
        (Err(operation_error), Err(restore_error)) => Err(operation_error.context(format!(
            "also failed to restore coordination board SQLite busy timeout: {restore_error:#}"
        ))),
    }
}

fn validate_coordination_board_set_input(
    input: &CoordinationBoardSetInput,
) -> Result<Option<String>> {
    validate_coordination_board_scope_id(&input.scope_project_id)?;
    validate_coordination_board_key(&input.key)?;
    validate_coordination_board_text_field(
        "author session id",
        &input.author_session_id,
        COORDINATION_BOARD_MAX_AUTHOR_ID_BYTES,
    )?;
    validate_coordination_board_text_field(
        "author name",
        &input.author_name,
        COORDINATION_BOARD_MAX_AUTHOR_NAME_BYTES,
    )?;
    validate_coordination_board_text_field(
        "idempotency key",
        &input.idempotency_key,
        COORDINATION_BOARD_MAX_IDEMPOTENCY_KEY_BYTES,
    )?;
    if let Some(state_stamp) = input.state_stamp.as_deref() {
        validate_coordination_board_text_field(
            "state stamp",
            state_stamp,
            COORDINATION_BOARD_MAX_STATE_STAMP_BYTES,
        )?;
    }
    input
        .value
        .as_ref()
        .map(canonical_coordination_board_value)
        .transpose()
}

fn validate_coordination_board_scope_id(scope_project_id: &str) -> Result<()> {
    validate_coordination_board_text_field(
        "scope project id",
        scope_project_id,
        COORDINATION_BOARD_MAX_SCOPE_ID_BYTES,
    )
}

fn validate_coordination_board_text_field(
    field: &str,
    value: &str,
    maximum_bytes: usize,
) -> Result<()> {
    if value.trim().is_empty() {
        return Err(coordination_board_store_error(
            CoordinationBoardStoreErrorKind::Validation,
            format!("coordination board {field} must not be empty"),
        ));
    }
    if value.len() > maximum_bytes {
        return Err(coordination_board_store_error(
            CoordinationBoardStoreErrorKind::Validation,
            format!("coordination board {field} exceeds the {maximum_bytes}-byte limit"),
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(coordination_board_store_error(
            CoordinationBoardStoreErrorKind::Validation,
            format!("coordination board {field} must not contain control characters"),
        ));
    }
    Ok(())
}

fn validate_coordination_board_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > COORDINATION_BOARD_MAX_KEY_BYTES {
        return Err(coordination_board_store_error(
            CoordinationBoardStoreErrorKind::Validation,
            format!(
                "coordination board key must contain 1 to {} bytes",
                COORDINATION_BOARD_MAX_KEY_BYTES
            ),
        ));
    }
    let segments = key.split('.').collect::<Vec<_>>();
    if segments.len() > 8
        || segments.iter().any(|segment| {
            let mut bytes = segment.bytes();
            !bytes
                .next()
                .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
                || !bytes.all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b'-')
                })
        })
    {
        return Err(coordination_board_store_error(
            CoordinationBoardStoreErrorKind::Validation,
            "coordination board keys must use 1 to 8 lowercase alphanumeric segments separated by \
             dots; `_` and `-` are allowed after the first character of a segment",
        ));
    }
    Ok(())
}

fn canonical_coordination_board_value(value: &Value) -> Result<String> {
    let depth = coordination_board_value_depth(value);
    if depth > COORDINATION_BOARD_MAX_VALUE_DEPTH {
        return Err(coordination_board_store_error(
            CoordinationBoardStoreErrorKind::Validation,
            format!(
                "coordination board value exceeds the maximum JSON depth of {}",
                COORDINATION_BOARD_MAX_VALUE_DEPTH
            ),
        ));
    }
    // Do not rely on serde_json::Map's current default backing map. Cargo
    // feature unification could enable `preserve_order` elsewhere in the
    // dependency graph, making otherwise identical object values hash
    // differently by insertion order. Rebuild every object from explicitly
    // sorted entries so canonical idempotency hashing remains stable under
    // either serde_json map representation.
    let canonical_value = sort_coordination_board_object_keys(value);
    let encoded = serde_json::to_string(&canonical_value)
        .context("failed to encode coordination board JSON value")?;
    if encoded.len() > COORDINATION_BOARD_MAX_VALUE_BYTES {
        return Err(coordination_board_store_error(
            CoordinationBoardStoreErrorKind::Validation,
            format!(
                "coordination board value exceeds the {}-byte canonical JSON limit",
                COORDINATION_BOARD_MAX_VALUE_BYTES
            ),
        ));
    }
    Ok(encoded)
}

fn sort_coordination_board_object_keys(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(sort_coordination_board_object_keys)
                .collect(),
        ),
        Value::Object(values) => {
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
            let mut sorted = serde_json::Map::new();
            for (key, value) in entries {
                sorted.insert(
                    key.clone(),
                    sort_coordination_board_object_keys(value),
                );
            }
            Value::Object(sorted)
        }
        scalar => scalar.clone(),
    }
}

fn coordination_board_value_depth(value: &Value) -> usize {
    // Do not recurse here. Wire JSON already has serde_json's parser depth
    // bound, but the store also accepts in-process Values and owns its own
    // validation contract.
    let mut maximum_depth = 0;
    let mut pending = vec![(value, 0usize)];
    while let Some((value, parent_depth)) = pending.pop() {
        let depth = parent_depth.saturating_add(1);
        match value {
            Value::Array(values) => {
                maximum_depth = maximum_depth.max(depth);
                pending.extend(values.iter().map(|child| (child, depth)));
            }
            Value::Object(values) => {
                maximum_depth = maximum_depth.max(depth);
                pending.extend(values.values().map(|child| (child, depth)));
            }
            _ => {}
        }
    }
    maximum_depth
}

fn coordination_board_request_hash(
    input: &CoordinationBoardSetInput,
    canonical_value: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();
    let expected_revision = input.expected_revision.to_string();
    for field in [
        input.scope_project_id.as_bytes(),
        input.key.as_bytes(),
        expected_revision.as_bytes(),
        canonical_value.unwrap_or("<tombstone>").as_bytes(),
        input.state_stamp.as_deref().unwrap_or("").as_bytes(),
    ] {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    format!("{:x}", hasher.finalize())
}

fn query_coordination_board_generation(
    connection: &rusqlite::Connection,
    scope_project_id: &str,
) -> Result<u64> {
    match connection.query_row(
        "SELECT generation
         FROM coordination_board_scopes
         WHERE scope_id = ?1",
        rusqlite::params![scope_project_id],
        |row| row.get::<_, i64>(0),
    ) {
        Ok(generation) => {
            u64::try_from(generation).context("coordination board generation must not be negative")
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
        Err(err) => Err(err).context("failed to read coordination board generation"),
    }
}

fn query_coordination_board_live_entry_count(
    connection: &rusqlite::Connection,
    scope_project_id: &str,
) -> Result<u64> {
    let count = connection
        .query_row(
            "SELECT COUNT(*)
             FROM coordination_board_entries
             WHERE scope_id = ?1 AND value_json IS NOT NULL",
            rusqlite::params![scope_project_id],
            |row| row.get::<_, i64>(0),
        )
        .context("failed to count live coordination board scope keys")?;
    u64::try_from(count).context("coordination board live-key count must not be negative")
}

fn query_coordination_board_distinct_key_count(
    connection: &rusqlite::Connection,
    scope_project_id: &str,
) -> Result<u64> {
    let count = connection
        .query_row(
            "SELECT COUNT(*)
             FROM coordination_board_entries
             WHERE scope_id = ?1",
            rusqlite::params![scope_project_id],
            |row| row.get::<_, i64>(0),
        )
        .context("failed to count distinct coordination board scope keys")?;
    u64::try_from(count).context("coordination board distinct-key count must not be negative")
}

fn query_coordination_board_head(
    connection: &rusqlite::Connection,
    scope_project_id: &str,
    key: &str,
) -> Result<Option<CoordinationBoardHead>> {
    match connection.query_row(
        "SELECT key, revision, generation, value_json,
                author_session_id, author_name, updated_at, state_stamp
         FROM coordination_board_entries
         WHERE scope_id = ?1 AND key = ?2",
        rusqlite::params![scope_project_id, key],
        coordination_board_head_from_row,
    ) {
        Ok(head) => Ok(Some(head)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(err) => Err(err).context("failed to read coordination board head"),
    }
}

fn coordination_board_head_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<CoordinationBoardHead> {
    let revision = row.get::<_, i64>(1)?;
    let generation = row.get::<_, i64>(2)?;
    let value_json = row.get::<_, Option<String>>(3)?;
    let deleted = value_json.is_none();
    let value = value_json
        .map(|encoded| {
            serde_json::from_str(&encoded).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    Box::new(err),
                )
            })
        })
        .transpose()?
        .unwrap_or(Value::Null);
    Ok(CoordinationBoardHead {
        key: row.get(0)?,
        revision: u64::try_from(revision).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Integer,
                Box::new(err),
            )
        })?,
        updated_at_generation: u64::try_from(generation).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Integer,
                Box::new(err),
            )
        })?,
        value,
        deleted,
        author_session_id: row.get(4)?,
        author_name: row.get(5)?,
        updated_at: row.get(6)?,
        state_stamp: row.get(7)?,
    })
}

fn query_coordination_board_entries(
    connection: &rusqlite::Connection,
    scope_project_id: &str,
    limit: i64,
) -> Result<Vec<CoordinationBoardHead>> {
    let mut statement = connection
        .prepare(
            "SELECT key, revision, generation, value_json,
                    author_session_id, author_name, updated_at, state_stamp
             FROM coordination_board_entries
             WHERE scope_id = ?1 AND value_json IS NOT NULL
             ORDER BY key
             LIMIT ?2",
        )
        .context("failed to prepare coordination board first-page query")?;
    collect_coordination_board_heads(statement.query_map(
        rusqlite::params![scope_project_id, limit],
        coordination_board_head_from_row,
    )?)
}

fn query_coordination_board_entries_after(
    connection: &rusqlite::Connection,
    scope_project_id: &str,
    after_key: &str,
    limit: i64,
) -> Result<Vec<CoordinationBoardHead>> {
    let mut statement = connection
        .prepare(
            "SELECT key, revision, generation, value_json,
                    author_session_id, author_name, updated_at, state_stamp
             FROM coordination_board_entries
             WHERE scope_id = ?1 AND value_json IS NOT NULL AND key > ?2
             ORDER BY key
             LIMIT ?3",
        )
        .context("failed to prepare coordination board continuation query")?;
    collect_coordination_board_heads(statement.query_map(
        rusqlite::params![scope_project_id, after_key, limit],
        coordination_board_head_from_row,
    )?)
}

fn collect_coordination_board_heads(
    rows: rusqlite::MappedRows<
        '_,
        impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<CoordinationBoardHead>,
    >,
) -> Result<Vec<CoordinationBoardHead>> {
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read coordination board entries")
}

fn query_coordination_board_idempotency(
    connection: &rusqlite::Connection,
    scope_project_id: &str,
    author_session_id: &str,
    idempotency_key: &str,
) -> Result<Option<(String, String)>> {
    match connection.query_row(
        "SELECT request_hash, receipt_json
         FROM coordination_board_idempotency
         WHERE scope_id = ?1
           AND author_session_id = ?2
           AND idempotency_key = ?3",
        rusqlite::params![scope_project_id, author_session_id, idempotency_key],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ) {
        Ok(value) => Ok(Some(value)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(err) => Err(err).context("failed to inspect coordination board idempotency receipt"),
    }
}

fn insert_coordination_board_history(
    connection: &rusqlite::Connection,
    scope_project_id: &str,
    current: &CoordinationBoardHead,
) -> Result<()> {
    let value_json = if current.deleted {
        None
    } else {
        Some(
            serde_json::to_string(&current.value)
                .context("failed to encode coordination board history value")?,
        )
    };
    let revision = i64::try_from(current.revision)
        .context("coordination board history revision exceeds SQLite range")?;
    let generation = i64::try_from(current.updated_at_generation)
        .context("coordination board history generation exceeds SQLite range")?;
    connection
        .execute(
            "INSERT INTO coordination_board_history(
               scope_id, key, revision, generation, value_json,
               author_session_id, author_name, updated_at, state_stamp
             )
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                scope_project_id,
                current.key,
                revision,
                generation,
                value_json,
                current.author_session_id,
                current.author_name,
                current.updated_at,
                current.state_stamp,
            ],
        )
        .context("failed to append coordination board history")?;
    Ok(())
}

fn trim_coordination_board_history(
    connection: &rusqlite::Connection,
    scope_project_id: &str,
    key: &str,
) -> Result<()> {
    let retention = i64::try_from(COORDINATION_BOARD_HISTORY_REVISIONS_PER_KEY)
        .expect("coordination board history retention fits in i64");
    connection
        .execute(
            "DELETE FROM coordination_board_history
             WHERE rowid IN (
               SELECT rowid
               FROM coordination_board_history
               WHERE scope_id = ?1 AND key = ?2
               ORDER BY revision DESC
               LIMIT -1 OFFSET ?3
             )",
            rusqlite::params![scope_project_id, key, retention],
        )
        .context("failed to compact coordination board history")?;
    Ok(())
}

fn purge_coordination_board_history(
    connection: &rusqlite::Connection,
    scope_project_id: &str,
    key: &str,
) -> Result<()> {
    connection
        .execute(
            "DELETE FROM coordination_board_history
             WHERE scope_id = ?1 AND key = ?2",
            rusqlite::params![scope_project_id, key],
        )
        .context("failed to purge deleted coordination board key history")?;
    Ok(())
}

fn trim_coordination_board_idempotency(
    transaction: &rusqlite::Transaction<'_>,
    scope_project_id: &str,
) -> Result<()> {
    let retention = i64::try_from(COORDINATION_BOARD_IDEMPOTENCY_RECEIPTS_PER_SCOPE)
        .expect("coordination board idempotency retention fits in i64");
    transaction
        .execute(
            "DELETE FROM coordination_board_idempotency
             WHERE rowid IN (
               SELECT rowid
               FROM coordination_board_idempotency
               WHERE scope_id = ?1
               ORDER BY rowid DESC
               LIMIT -1 OFFSET ?2
             )",
            rusqlite::params![scope_project_id, retention],
        )
        .context("failed to trim coordination board idempotency receipts")?;
    Ok(())
}

#[cfg(test)]
#[path = "coordination_board_tests.rs"]
mod coordination_board_tests;
