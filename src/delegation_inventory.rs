// Owns full-inventory startup loading, atomic publication and completeness gates.
// Does not persist history or decide ready-state delegation/root authority.
// Split from boot loading so every consumer can distinguish unknown from empty.

#[derive(Clone, Debug, Default)]
enum DelegationInventoryState {
    #[default]
    Ready,
    Loading {
        token: String,
        source: FederationImageFileIdentity,
        worker_started: bool,
    },
    Failed {
        reason: &'static str,
    },
}

fn delegation_inventory_file_identity(path: &FsPath) -> Result<FederationImageFileIdentity> {
    let file = fs::File::open(path).context("opening delegation inventory source")?;
    Ok(federation_image_file_identity(&file, &file.metadata()?)?)
}

fn verify_delegation_inventory_source(
    connection: &rusqlite::Connection,
    path: &FsPath,
    source: &FederationImageFileIdentity,
) -> Result<()> {
    anyhow::ensure!(
        delegation_inventory_file_identity(path)? == *source,
        "delegation inventory source was replaced before publication"
    );
    #[cfg(unix)]
    {
        let opened_path = connection.path()
            .context("delegation inventory connection source pathname unavailable")?;
        anyhow::ensure!(delegation_inventory_file_identity(FsPath::new(opened_path))? == *source,
            "delegation inventory connection source pathname differs from captured source");
        let mut moved: std::ffi::c_int = 0;
        // SAFETY: the connection is exclusively borrowed on this worker, main
        // names its live database, and HAS_MOVED writes one initialized int.
        // The bundled Unix VFS compares its opened inode to its pathname.
        let status = unsafe {
            rusqlite::ffi::sqlite3_file_control(
                connection.handle(), c"main".as_ptr(),
                rusqlite::ffi::SQLITE_FCNTL_HAS_MOVED,
                (&mut moved as *mut std::ffi::c_int).cast(),
            )
        };
        anyhow::ensure!(status == rusqlite::ffi::SQLITE_OK,
            "delegation inventory connection source check unavailable ({status})");
        anyhow::ensure!(moved == 0,
            "delegation inventory connection source no longer matches its pathname");
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::{BorrowedHandle, RawHandle};
        let mut handle: RawHandle = std::ptr::null_mut();
        // SAFETY: the connection stays live and exclusively borrowed; SQLite's
        // Windows VFS writes its borrowed native database handle to this slot.
        let status = unsafe {
            rusqlite::ffi::sqlite3_file_control(
                connection.handle(), c"main".as_ptr(),
                rusqlite::ffi::SQLITE_FCNTL_WIN32_GET_HANDLE,
                (&mut handle as *mut RawHandle).cast(),
            )
        };
        anyhow::ensure!(status == rusqlite::ffi::SQLITE_OK && !handle.is_null()
            && handle as isize != -1,
            "delegation inventory connection source handle unavailable ({status})");
        // SAFETY: SQLite owns this valid handle throughout the borrow. Clone
        // it before constructing File, so File never closes SQLite's handle.
        let owned = unsafe { BorrowedHandle::borrow_raw(handle) }.try_clone_to_owned()?;
        let file = fs::File::from(owned);
        anyhow::ensure!(federation_image_file_identity(&file, &file.metadata()?)? == *source,
            "delegation inventory connection source differs from captured source");
    }
    Ok(())
}

fn delegation_inventory_hash_row(hash: &mut sha2::Sha256, id: &str, encoded: &str) {
    hash.update((id.len() as u64).to_le_bytes());
    hash.update(id.as_bytes());
    hash.update((encoded.len() as u64).to_le_bytes());
    hash.update(encoded.as_bytes());
}

fn delegation_inventory_fingerprint(connection: &rusqlite::Connection) -> Result<String> {
    let mut statement =
        connection.prepare("SELECT id,value_json FROM delegations ORDER BY rowid")?;
    let mut rows = statement.query([])?;
    let mut hash = sha2::Sha256::new();
    while let Some(row) = rows.next()? {
        delegation_inventory_hash_row(
            &mut hash,
            &row.get::<_, String>(0)?,
            &row.get::<_, String>(1)?,
        );
    }
    Ok(format!("{:x}", hash.finalize()))
}

impl StateInner {
    fn delegation_inventory_complete(&self) -> bool {
        matches!(self.delegation_inventory, DelegationInventoryState::Ready)
    }

    fn require_delegation_inventory(&self) -> Result<(), ApiError> {
        if self.delegation_inventory_complete() {
            return Ok(());
        }
        let message = match &self.delegation_inventory {
            DelegationInventoryState::Loading { .. } => {
                "delegation inventory is starting up; retry when startup completes".to_owned()
            }
            DelegationInventoryState::Failed { reason } => {
                format!("delegation inventory is unavailable ({reason}); inspect the startup error")
            }
            DelegationInventoryState::Ready => unreachable!("handled above"),
        };
        Err(ApiError::from_status(
            StatusCode::SERVICE_UNAVAILABLE,
            message,
        ))
    }
}

// SQL and decoding live on the post-listen OS worker, never the reactor or
// StateInner. Only the full owned result crosses the publication boundary.
impl AppState {
    #[cfg(test)]
    fn load_delegation_inventory_after_listen(&self) -> Result<()> {
        self.load_delegation_inventory_observed(|_| {}).map(|_| ())
    }

    fn load_delegation_inventory_observed(
        &self,
        mut observe: impl FnMut(&str),
    ) -> Result<Option<EngramBootRecoveryPlan>> {
        let (token, source) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            match &mut inner.delegation_inventory {
                DelegationInventoryState::Ready => return Ok(None),
                DelegationInventoryState::Loading {
                    token,
                    source,
                    worker_started,
                } if !*worker_started => {
                    *worker_started = true;
                    (token.clone(), source.clone())
                }
                _ => anyhow::bail!("delegation inventory is unavailable or already loading"),
            }
        };
        let started = std::time::Instant::now();
        let mut read_connection = None;
        let loaded = (|| -> Result<_> {
            observe("before_sql");
            anyhow::ensure!(
                delegation_inventory_file_identity(&self.persistence_path)? == source,
                "delegation inventory source was replaced before loading"
            );
            observe("source_checked");
            let connection = rusqlite::Connection::open_with_flags(
                self.persistence_path.as_path(),
                // No rows are written. Read/write access permits the final
                // IMMEDIATE reservation on this same connection/version clock.
                rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
            )?;
            observe("connection_opened");
            verify_delegation_inventory_source(&connection, &self.persistence_path, &source)?;
            connection.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
            connection.execute_batch("BEGIN")?;
            let version: i64 = connection.query_row("PRAGMA data_version", [], |row| row.get(0))?;
            let (records, quarantined, fingerprint) = load_delegation_records_with_fingerprint(
                &connection,
                &self.persistence_path,
                || *self.shutdown_signal_tx.borrow(),
            )?;
            let running = records
                .iter()
                .enumerate()
                .filter_map(|(index, record)| {
                    running_read_only_delegation_index_entry(record, index)
                })
                .collect::<BTreeSet<_>>();
            let links = records
                .iter()
                .map(|record| (record.child_session_id.clone(), record.id.clone()))
                .collect::<BTreeMap<_, _>>();
            let catalog_children = FederationCatalogChildren::from_records(&records);
            observe("after_decode");
            connection.execute_batch("COMMIT")?;
            verify_delegation_inventory_source(&connection, &self.persistence_path, &source)?;
            anyhow::ensure!(
                !*self.shutdown_signal_tx.borrow(),
                "delegation inventory load interrupted by shutdown"
            );
            read_connection = Some(connection);
            Ok((records, quarantined, running, links, catalog_children, version, fingerprint))
        })();
        observe("before_publish");
        let writer = sqlite_state_write_lock(&self.persistence_path);
        let mut publication = None;
        let mut loaded = loaded;
        let mut inner = if let Ok((_, _, _, _, _, version, fingerprint)) = &loaded {
            match self.lock_delegation_inventory_publication(
                read_connection.as_ref().expect("successful inventory owns its connection"),
                &writer, *version, fingerprint, &source, &mut observe,
            ) {
                Ok((inner, transaction, admission)) => {
                    publication = Some((transaction, admission));
                    inner
                }
                Err(error) => {
                    loaded = Err(error);
                    self.inner.lock().expect("state mutex poisoned")
                }
            }
        } else {
            self.inner.lock().expect("state mutex poisoned")
        };
        anyhow::ensure!(
            matches!(&inner.delegation_inventory,
            DelegationInventoryState::Loading { token: current, .. } if current == &token),
            "delegation inventory owner changed before publication"
        );
        match loaded {
            Ok((records, quarantined, running, links, catalog_children, _, _)) => {
                if !inner.delegations.is_empty()
                    || !inner.delegation_mutation_stamps.is_empty()
                    || !inner.removed_delegation_ids.is_empty()
                    || *self.shutdown_signal_tx.borrow()
                {
                    inner.delegation_inventory = DelegationInventoryState::Failed {
                        reason: "publicationConflict",
                    };
                    anyhow::bail!(
                        "delegation inventory publication conflicted with a writer or shutdown"
                    );
                }
                inner.delegations = records;
                inner.quarantined_persisted_delegation_ids = quarantined;
                inner.running_read_only_delegations = running;
                *inner.delegation_catalog_children.get_mut().expect("catalog child index poisoned") =
                    catalog_children;
                inner.repair_delegation_child_session_links_with(links);
                inner.delegation_inventory = DelegationInventoryState::Ready;
                // Publication is linearized while external SQL writers are
                // excluded. Release before recovery can synchronously persist.
                drop(publication.take());
                inner.recover_interrupted_delegations();
                let revision = inner.revision;
                let prepared = self
                    .prepare_engram_sessions_for_boot_recovery_locked(&mut inner)
                    .and_then(|plan| {
                        if inner.revision == revision {
                            self.commit_locked(&mut inner)?;
                        }
                        Ok(plan)
                    });
                let plan = match prepared {
                    Ok(plan) => plan,
                    Err(error) => {
                        inner.delegation_inventory = DelegationInventoryState::Failed {
                            reason: "recoveryPreparationFailed",
                        };
                        drop(inner);
                        eprintln!("startup> inventory recovery preparation failed: {error:#}");
                        return Err(error);
                    }
                };
                let count = inner.delegations.len();
                drop(inner);
                drop(self.engram_retry_slots.release_boot_hold_on_drop());
                eprintln!(
                    "startup> delegation inventory complete rows={count} elapsed_ms={}",
                    started.elapsed().as_millis()
                );
                Ok(Some(plan))
            }
            Err(error) => {
                inner.delegation_inventory = DelegationInventoryState::Failed {
                    reason: "loadFailed",
                };
                drop(inner);
                eprintln!(
                    "startup> delegation inventory unavailable elapsed_ms={} error={error:#}",
                    started.elapsed().as_millis()
                );
                Err(error)
            }
        }
    }
}

/// Requests which can classify, mutate or consume delegation history must not
/// turn an unloaded Vec into absence. Metadata-only GET routes remain live.
async fn delegation_inventory_http_guard(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let path = request.uri().path();
    let independent_io =
        path == "/api/file" || path.starts_with("/api/git/") || path.starts_with("/api/terminal/");
    let depends_on_inventory = (!independent_io && request.method() != axum::http::Method::GET)
        || path.starts_with("/api/sessions")
        || path == "/api/state"
        || path == "/api/events"
        || path.starts_with("/api/federation/mailbox");
    if depends_on_inventory {
        let unavailable = {
            let inner = state.inner.lock().expect("state mutex poisoned");
            inner.require_delegation_inventory().err().map(|error| {
                let code = if matches!(
                    inner.delegation_inventory,
                    DelegationInventoryState::Loading { .. }
                ) {
                    "delegation_inventory_starting"
                } else {
                    "delegation_inventory_unavailable"
                };
                (error, code)
            })
        };
        if let Some((error, code)) = unavailable {
            return (
                error.status,
                [(axum::http::header::RETRY_AFTER, "1")],
                Json(json!({"error": error.message, "code": code})),
            )
                .into_response();
        }
    }
    next.run(request).await
}
