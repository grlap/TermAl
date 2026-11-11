// Owns the local durable publication cut for prepared mailbox descriptors.
// Reuses the host state database and writer admission; grants no remote use.

const FEDERATION_REFERENCE_PUBLICATION_PREFIX: &str = "federation_reference_publication_v1:";

const FEDERATION_REFERENCE_PUBLICATION_TIMEOUT: Duration = Duration::from_secs(60);
const FEDERATION_REFERENCE_PUBLICATION_WARNING: Duration = Duration::from_secs(10);
const FEDERATION_REFERENCE_PUBLICATION_BACKOFF: Duration = Duration::from_millis(25);

fn reference_publication_unsettled() -> ApiError {
    ApiError::internal("reference publication outcome unresolved; retry the identical intent and key")
}

fn reference_publication_sql<T>(
    result: Result<T>, contended: &mut bool,
) -> Result<T, ApiError> {
    result.map_err(|error| {
        *contended = error.chain().any(|cause| {
            matches!(cause.downcast_ref::<rusqlite::Error>(),
                Some(rusqlite::Error::SqliteFailure(code, _))
                    if code.code == rusqlite::ErrorCode::DatabaseBusy)
        });
        reference_publication_unsettled()
    })
}

// A failed COMMIT leaves the connection-owned transaction open until rollback
// is acknowledged. No rusqlite Transaction Drop participates in this decision.
// Autocommit after an attempted COMMIT is not proof that it failed: it must
// take the exact-marker reconciliation path instead of replaying the write.
fn rollback_reference_publication(
    connection: &rusqlite::Connection,
    sql: &mut impl FnMut(&rusqlite::Connection, &'static str) -> rusqlite::Result<()>,
) -> bool {
    !connection.is_autocommit()
        && reference_publication_observe!("publish.rollback", sql(connection, "ROLLBACK")).is_ok()
        && connection.is_autocommit()
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationReferencePublication {
    schema_version: u16,
    reference_id: String,
    generation: u64,
    owner: String,
    owner_creation: String,
    intent: String,
    digest: String,
}

fn federation_reference_publication_key(id: &str) -> String {
    format!("{FEDERATION_REFERENCE_PUBLICATION_PREFIX}{id}")
}

fn read_federation_reference_publication(
    connection: &rusqlite::Connection, id: &str,
) -> Result<Option<FederationReferencePublication>> {
    let encoded: Option<String> = connection.query_row(
        "SELECT value FROM meta WHERE key=?1",
        [federation_reference_publication_key(id)], |row| row.get(0),
    ).optional()?;
    encoded.map(|value| serde_json::from_str(&value).map_err(Into::into)).transpose()
}

fn committed_federation_reference_publication(
    connection: &rusqlite::Connection, row: &FederationMailboxReference,
) -> Result<Option<FederationReferencePublication>, ApiError> {
    let unavailable = || ApiError::internal("reference publication lookup unresolved; retry the identical key");
    let receipt = reference_publication_observe!("lookup.marker", read_federation_reference_publication(connection, &row.id)).map_err(|error| {
        if error.downcast_ref::<serde_json::Error>().is_some() {
            ApiError::conflict("reference publication marker malformed")
        } else { unavailable() }
    })?;
    let Some(receipt) = receipt else {
        return Ok(None);
    };
    let expected = reference_publication_observe!("lookup.expected", row.publication_marker()).map_err(|_| unavailable())?;
    let image = reference_publication_observe!("lookup.image", read_federation_persist_image(connection)).map_err(|_| unavailable())?
        .ok_or_else(|| {
            #[cfg(test)]
            reference_publication_observe_error("lookup.image_missing", &"no committed image");
            unavailable()
        })?;
    reference_publication_observe!("lookup.image_marker", verify_federation_image_marker(connection, &image)).map_err(|_| unavailable())?;
    if image["domain"]["local"] != serde_json::to_value(
        reference_publication_observe!("lookup.host", read_federation_host_identity(connection)).map_err(|_| unavailable())?
    ).map_err(|_| unavailable())? {
        #[cfg(test)]
        reference_publication_observe_error("lookup.host_mismatch", &"image host differs from durable host");
        return Err(unavailable());
    }
    let prepared: FederationMailboxReference = serde_json::from_value(
        image["domain"]["mailboxReferences"][&row.id].clone(),
    ).map_err(|_| ApiError::conflict("reference publication prepared content missing or invalid"))?;
    if receipt != expected || prepared.publication_marker().map_err(|_| unavailable())? != expected {
        return Err(ApiError::conflict("reference publication marker conflicts with prepared content"));
    }
    Ok(Some(receipt))
}

fn restore_federation_reference_publications(
    inner: &mut StateInner, connection: &rusqlite::Connection,
) -> Result<()> {
    for row in inner.federation.mailbox_references.values_mut() {
        let receipt = read_federation_reference_publication(connection, &row.id)?;
        let expected = row.publication_marker()?;
        // Old-format prepared rows are not backfilled. Conflicting or unknown
        // receipt versions cannot open a different descriptor's visibility.
        row.publication = receipt.filter(|receipt| *receipt == expected);
    }
    Ok(())
}

fn federation_reference_publication_busy() -> ApiError {
    ApiError::from_status(StatusCode::SERVICE_UNAVAILABLE,
        "REFERENCE_PUBLICATION_BUSY: local publication admission busy; retry the identical intent and key")
}

impl AppState {
    fn reconcile_federation_reference_publication(
        &self, row: &FederationMailboxReference,
    ) -> Result<Option<Value>, ApiError> {
        let receipt = {
            let connection = reference_publication_observe!("reconcile.open_reader", open_sqlite_state_read_connection(self.persistence_path.as_path()))
                .map_err(|_| ApiError::internal("reference publication lookup unavailable; retry the identical key"))?;
            let transaction = reference_publication_observe!("reconcile.begin_reader", rusqlite::Transaction::new_unchecked(
                &connection, rusqlite::TransactionBehavior::Deferred,
            )).map_err(|_| ApiError::internal("reference publication lookup unavailable; retry the identical key"))?;
            committed_federation_reference_publication(&transaction, row)?
        };
        let Some(receipt) = receipt else { return Ok(None); };
        // Historical replay is not a new proof or new publication. The durable
        // cut already happened; this cannot retarget or grant current use.
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let owner = federation_mailbox_root_locked(&inner, &row.owner)?;
        if owner.federation_native_creation_id != row.owner_creation
            || owner.mailbox_binding() != Some(&row.owner_binding)
        {
            return Err(ApiError::conflict("reference owner namespace changed during replay"));
        }
        let current = inner.federation.mailbox_references.get_mut(&row.id)
            .ok_or_else(|| ApiError::conflict("reference disappeared during publication replay"))?;
        if !current.same_prepared(row) {
            return Err(ApiError::conflict("reference changed during publication replay"));
        }
        current.publication = Some(receipt);
        Ok(Some(current.descriptor()))
    }

    fn publish_federation_reference(
        &self, context: &FederationReferenceContext, row: &FederationMailboxReference,
    ) -> Result<Value, ApiError> {
        let started = std::time::Instant::now();
        self.publish_federation_reference_with_wait(
            context, row, || started.elapsed(), std::thread::sleep,
            |spent| eprintln!("reference publication warning> operation elapsed {} ms", spent.as_millis()),
            |connection, statement| connection.execute_batch(statement),
        )
    }

    fn publish_federation_reference_with_wait(
        &self, context: &FederationReferenceContext, row: &FederationMailboxReference,
        mut elapsed: impl FnMut() -> Duration,
        mut pause: impl FnMut(Duration),
        mut warn: impl FnMut(Duration),
        mut sql: impl FnMut(&rusqlite::Connection, &'static str) -> rusqlite::Result<()>,
    ) -> Result<Value, ApiError> {
        let mut warned = false;
        loop {
            let spent = elapsed();
            if !warned && spent >= FEDERATION_REFERENCE_PUBLICATION_WARNING {
                warned = true;
                warn(spent);
            }
            if spent >= FEDERATION_REFERENCE_PUBLICATION_TIMEOUT {
                return Err(federation_reference_publication_busy());
            }
            let attempt = self.try_publish_federation_reference(context, row, &mut elapsed, &mut sql)?;
            let spent = elapsed();
            if !warned && spent >= FEDERATION_REFERENCE_PUBLICATION_WARNING {
                warned = true;
                warn(spent);
            }
            if let Some(descriptor) = attempt {
                return Ok(descriptor);
            }
            if spent >= FEDERATION_REFERENCE_PUBLICATION_TIMEOUT {
                return Err(federation_reference_publication_busy());
            }
            // The attempt has released every link/state guard, transaction,
            // connection and writer ticket. Re-enter with the original budget
            // and exact prepared identity; never retarget a stale descriptor.
            let remaining = FEDERATION_REFERENCE_PUBLICATION_TIMEOUT - spent;
            let until_warning = if warned { remaining } else {
                FEDERATION_REFERENCE_PUBLICATION_WARNING.saturating_sub(spent)
            };
            pause(FEDERATION_REFERENCE_PUBLICATION_BACKOFF.min(remaining).min(until_warning));
        }
    }

    fn try_publish_federation_reference(
        &self, context: &FederationReferenceContext, row: &FederationMailboxReference,
        elapsed: &mut impl FnMut() -> Duration,
        sql: &mut impl FnMut(&rusqlite::Connection, &'static str) -> rusqlite::Result<()>,
    ) -> Result<Option<Value>, ApiError> {
        let unsettled = reference_publication_unsettled;
        let path = self.persistence_path.as_path();
        let expected = reference_publication_observe!("publish.expected", row.publication_marker()).map_err(|_| unsettled())?;
        // All blocking path/schema/open/write-reservation work is OUTSIDE the
        // final authority locks. The same writer discipline protects snapshots.
        reference_publication_observe!("publish.path_before_open", reject_existing_sqlite_state_path_redirection(path)).map_err(|_| unsettled())?;
        let connection = reference_publication_observe!("publish.open_writer", open_sqlite_state_connection_unconfigured(path)).map_err(|_| unsettled())?;
        reference_publication_observe!("publish.zero_busy_timeout", connection.busy_timeout(Duration::ZERO)).map_err(|_| unsettled())?;
        let writer = sqlite_state_write_lock(path);
        let Some(admission) = lock_sqlite_state_writer_for(&writer, Duration::ZERO) else { return Ok(None); };
        let setup = ensure_sqlite_state_schema(&connection);
        let mut contended = false;
        if let Err(error) = reference_publication_sql(
            reference_publication_observe!("publish.schema_and_hardening", finish_sqlite_state_file_setup(path, setup)), &mut contended,
        ) {
            let retry = contended && (connection.is_autocommit() || rollback_reference_publication(&connection, sql));
            drop(connection);
            drop(admission);
            return if retry { Ok(None) } else { Err(error) };
        }
        reference_publication_observe!("publish.path_before_begin", reject_existing_sqlite_state_path_redirection(path)).map_err(|_| unsettled())?;
        if let Err(error) = reference_publication_observe!("publish.begin_writer", connection.execute_batch("BEGIN IMMEDIATE")) {
            let retry = matches!(&error, rusqlite::Error::SqliteFailure(code, _)
                if code.code == rusqlite::ErrorCode::DatabaseBusy) && connection.is_autocommit();
            let failure = match error {
                rusqlite::Error::SqliteFailure(code, _) if code.code == rusqlite::ErrorCode::DatabaseLocked
                    => federation_reference_publication_busy(),
                _ => unsettled(),
            };
            drop(connection);
            drop(admission);
            return if retry { Ok(None) } else { Err(failure) };
        }
        let preparation = (|| -> Result<(), ApiError> {
            // The reservation excludes all other writers. Validate exact prepared
            // bytes and their existing content marker before taking authority locks.
            let image = reference_publication_sql(reference_publication_observe!("publish.image", read_federation_persist_image(&connection)), &mut contended)?
                .ok_or_else(|| {
                    #[cfg(test)]
                    reference_publication_observe_error("publish.image_missing", &"no committed image");
                    unsettled()
                })?;
            reference_publication_sql(reference_publication_observe!("publish.image_marker", verify_federation_image_marker(&connection, &image)), &mut contended)?;
            let host = reference_publication_sql(reference_publication_observe!("publish.host", read_federation_host_identity(&connection)), &mut contended)?;
            if image["domain"]["local"] != serde_json::to_value(host).map_err(|_| unsettled())? {
                #[cfg(test)]
                reference_publication_observe_error("publish.host_mismatch", &"image host differs from durable host");
                return Err(unsettled());
            }
            let prepared: FederationMailboxReference = serde_json::from_value(
                image["domain"]["mailboxReferences"][&row.id].clone(),
            ).map_err(|_| ApiError::conflict("prepared reference is not committed"))?;
            if prepared.publication_marker().map_err(|_| unsettled())? != expected {
                return Err(ApiError::conflict("committed reference content changed"));
            }
            let existing = reference_publication_sql(reference_publication_observe!("publish.existing_marker", read_federation_reference_publication(&connection, &row.id)), &mut contended)?;
            if existing.as_ref().is_some_and(|receipt| *receipt != expected) {
                return Err(ApiError::conflict("reference publication marker conflicts"));
            }
            let encoded = reference_publication_observe!("publish.encode_marker", serde_json::to_string(&expected)).map_err(|_| unsettled())?;
            reference_publication_sql(reference_publication_observe!("publish.insert_marker", connection.execute(
                "INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO NOTHING",
                rusqlite::params![federation_reference_publication_key(&row.id), encoded],
            )).map_err(Into::into), &mut contended)?;
            Ok(())
        })();
        if let Err(error) = preparation {
            let rolled_back = rollback_reference_publication(&connection, sql);
            drop(connection);
            drop(admission);
            return if contended && rolled_back { Ok(None) } else { Err(error) };
        }
        let mut commit_uncertain = false;
        let mut commit_busy = false;
        let mut expired = false;
        let result = context.try_with_publication_authority(self, |inner| {
            if elapsed() >= FEDERATION_REFERENCE_PUBLICATION_TIMEOUT {
                expired = true;
                return Err(federation_reference_publication_busy());
            }
            let current = inner.federation.mailbox_references.get_mut(&row.id)
                .ok_or_else(|| ApiError::conflict("reference disappeared before publication"))?;
            if !current.same_prepared(row) {
                return Err(ApiError::conflict("reference changed before publication"));
            }
            // The successful small local commit is the publication cut, while
            // link/state guards still exclude contradictory authority changes.
            // This connection alone owns BEGIN/COMMIT/ROLLBACK. No consuming
            // Transaction::commit or implicit rollback decides a retry.
            let committed = match reference_publication_observe!("publish.commit", sql(&connection, "COMMIT")) {
                Ok(()) => connection.is_autocommit(),
                Err(rusqlite::Error::SqliteFailure(code, _)) if code.code == rusqlite::ErrorCode::DatabaseBusy => {
                    commit_busy = true;
                    return Err(federation_reference_publication_busy());
                }
                Err(_) => false,
            };
            #[cfg(test)]
            let committed = committed && !context.handle.channel.reference_publication_ack_lost
                .swap(false, Ordering::SeqCst);
            if !committed {
                #[cfg(test)]
                reference_publication_observe_error("publish.commit_unconfirmed", &"COMMIT failed or test ACK was lost; raw COMMIT error, if any, precedes this line");
                commit_uncertain = true;
                return Err(unsettled());
            }
            current.publication = Some(expected.clone());
            Ok(current.descriptor())
        });
        // A failed TRY never waits while holding this reservation/admission.
        // Explicit rollback and connection close precede writer release.
        let retry = matches!(&result, Ok(None)) || commit_busy || expired;
        let rolled_back = !connection.is_autocommit() && rollback_reference_publication(&connection, sql);
        if retry && !rolled_back { commit_uncertain = true; }
        drop(connection);
        drop(admission);
        if retry && !commit_uncertain { return Ok(None); }
        if commit_uncertain {
            // Commit may have reached disk despite the error. Only an exact
            // durable lookup can reconcile it; never mint a replacement handle.
            // Never read through a connection whose failed COMMIT/ROLLBACK
            // might still expose its own uncommitted insert.
            let observed = {
                let reader = reference_publication_observe!("uncertain.open_reader", open_sqlite_state_read_connection(path)).map_err(|_| unsettled())?;
                let transaction = reference_publication_observe!("uncertain.begin_reader", rusqlite::Transaction::new_unchecked(
                    &reader, rusqlite::TransactionBehavior::Deferred,
                )).map_err(|_| unsettled())?;
                reference_publication_observe!("uncertain.lookup", committed_federation_reference_publication(&transaction, row)).map_err(|_| unsettled())?
            };
            if observed.as_ref() == Some(&expected) {
                reference_publication_observe!("uncertain.integrity", verify_persist_commit_integrity(path)).map_err(|_| unsettled())?;
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                let owner = federation_mailbox_root_locked(&inner, &row.owner)?;
                if owner.federation_native_creation_id != row.owner_creation
                    || owner.mailbox_binding() != Some(&row.owner_binding)
                {
                    #[cfg(test)]
                    reference_publication_observe_error("uncertain.owner_changed", &"owner changed after receipt readback");
                    return Err(unsettled());
                }
                if let Some(current) = inner.federation.mailbox_references.get_mut(&row.id) {
                    if current.same_prepared(row) {
                        current.publication = Some(expected);
                        return Ok(Some(current.descriptor()));
                    }
                }
            }
            #[cfg(test)]
            reference_publication_observe_error("uncertain.receipt_unconfirmed", &"no exact receipt and current prepared row");
            return Err(unsettled());
        }
        let descriptor = reference_publication_observe!("publish.authority_or_cut", result)?.ok_or_else(unsettled)?;
        reference_publication_observe!("publish.final_integrity", verify_persist_commit_integrity(path)).map_err(|_| unsettled())?;
        // A later link loss is a later event, not retroactive publication
        // failure. Resolve/send retain their separate fresh authority checks.
        Ok(Some(descriptor))
    }
}

impl FederationReferenceContext {
    #[cfg(test)]
    fn with_publication_authority<T>(
        &self, app: &AppState,
        publish: impl FnOnce(&mut StateInner) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        self.try_with_publication_authority(app, publish)?.ok_or_else(federation_reference_publication_busy)
    }

    fn try_with_publication_authority<T>(
        &self, app: &AppState,
        publish: impl FnOnce(&mut StateInner) -> Result<T, ApiError>,
    ) -> Result<Option<T>, ApiError> {
        // Some old synchronous writers hold state while waiting for writer
        // admission. TRY-only link -> state avoids an inverse blocking cycle.
        let state = match self.lease.channel.state.try_lock() {
            Ok(state) => state,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(None),
            Err(std::sync::TryLockError::Poisoned(_)) => return Err(ApiError::internal("publication link integrity unavailable")),
        };
        if state.incarnation.as_deref() != Some(self.lease.incarnation.as_str()) {
            return Err(federation_stale_link());
        }
        let Some(mut inner) = app.inner.try_lock_now() else { return Ok(None); };
        if app.inner.inner.is_poisoned() {
            return Err(ApiError::internal("publication state integrity unavailable"));
        }
        // Captured exact authority avoids a nested basis-lock acquisition.
        // Refresh and check are lock-free over the already guarded state.
        federation_reference_settled_locked(&inner)?;
        app.refresh_federation_transport_snapshot_locked(&inner, &self.peer, &self.lease.incarnation)?;
        self.check(app, &inner)?;
        publish(&mut inner).map(Some)
    }
}
