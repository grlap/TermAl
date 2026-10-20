// Local operator diagnostics. Reads share one SQLite snapshot per page and
// never initialize storage, refresh reservations or dispatch work.
async fn get_federation_journal(
    State(state): State<AppState>,
    query: std::result::Result<Query<FederationJournalQuery>, QueryRejection>,
) -> FederationJournalResult<Json<Value>> {
    let Query(query) = query.map_err(|_| FederationJournalError::Invalid)?;
    query.validate()?;
    tokio::task::spawn_blocking(move || state.federation_journal_view(query))
        .await
        .map_err(|_| FederationJournalError::Storage)?
        .map(Json)
}

impl AppState {
    fn federation_journal_view(
        &self,
        query: FederationJournalQuery,
    ) -> FederationJournalResult<Value> {
        let (stream, limit) = query.validate()?;
        let snapshot = federation_journal_snapshot(self)?;
        if stream == FederationJournalStream::Wakes && snapshot.role != FederationRole::Central {
            return Err(FederationJournalError::NonCentral(snapshot.role));
        }
        let cursor = query
            .cursor
            .as_deref()
            .map(|text| FederationJournalCursor::decode(text, stream, &snapshot.basis))
            .transpose()?;
        let page = {
            let Some(mut connection) = self.mailbox_store.connection_if_enabled() else {
                if cursor.is_some() {
                    return Err(FederationJournalError::Restart);
                }
                return Ok(json!({"stream":stream,"entries":[],
                    "nextCursor":null,"dispatchScan":null}));
            };
            let transaction = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)
                .map_err(|_| FederationJournalError::Storage)?;
            let high = if let Some(cursor) = &cursor {
                federation_journal_validate_anchor(&transaction, stream, &cursor.high)?;
                federation_journal_validate_anchor(&transaction, stream, &cursor.previous)?;
                Some(cursor.high.clone())
            } else {
                federation_journal_anchor(&transaction, stream, None)?
            };
            let previous = cursor.as_ref().map(|cursor| cursor.previous.rowid);
            let mut rows = match (&high, stream) {
                (Some(high), FederationJournalStream::Operations) => {
                    federation_journal_operation_rows(
                        &transaction,
                        &snapshot.local.host_id,
                        high.rowid,
                        previous,
                        limit,
                    )?
                }
                (Some(high), FederationJournalStream::Wakes) => {
                    let wakes = federation_journal_wake_rows(
                        &transaction,
                        &snapshot.local.host_id,
                        high.rowid,
                        previous,
                        limit,
                    )?;
                    snapshot.scan.project(&transaction, wakes)?
                }
                (None, _) => Vec::new(),
            };
            let more = rows.len() > limit;
            rows.truncate(limit);
            let next = if more {
                Some(
                    FederationJournalCursor {
                        version: 1,
                        stream,
                        basis: snapshot.basis.clone(),
                        high: high.expect("nonempty journal page has a high anchor"),
                        previous: rows.last().expect("positive page limit").0.clone(),
                    }
                    .encode()?,
                )
            } else {
                None
            };
            let entries: Vec<_> = rows.into_iter().map(|(_, row)| row).collect();
            let page = json!({"stream":stream,"entries":entries,"nextCursor":next,
                "dispatchScan":snapshot.scan.summary});
            transaction
                .commit()
                .map_err(|_| FederationJournalError::Storage)?;
            page
        };
        // A domain change while the page was read cannot mix an old cursor
        // basis with a current response. Release the DB guard before this lock.
        if federation_journal_basis(self)?.0 != snapshot.basis {
            return Err(FederationJournalError::Restart);
        }
        Ok(page)
    }
}
