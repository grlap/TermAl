// Durable scheduling metadata on the original send rows. Custody, authenticated
// completion and final receipts remain with their existing owners.
const CAUSAL_PENDING_DIAGNOSTICS: usize = 64;
const CAUSAL_PENDING_METADATA_REASON: &str = "pending_metadata_decode";
const CAUSAL_PENDING_METADATA_RELEASE: &str = "pending_metadata_readable";

fn causal_pending_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum CausalPendingReason {
    AwaitingReceipt,
    AwaitingCompletion,
    TestRequestDecode,
    RowIdentityDecode,
    ReceiptDecode,
    SelectorValidate,
    ScopeDecode,
    PeerDecode,
    SettlementDecodeOrValidate,
    OriginalCustodyChanged,
    CurrentnessCheckFailed,
    RunnerUnavailable,
    LeaseUnavailable,
    RouteChanged,
    OriginTimeout,
    OriginReadFailed,
    CompletionValidate,
    CompletionNotTerminal,
    CompletionOriginMismatch,
    CompletionRunMismatch,
    CompletionInputUnsupported,
    CompletionModeMismatch,
    PostReadRouteChanged,
    PostReadCurrentnessCheckFailed,
    AdmissionPending,
    AdmissionFailed,
}

impl CausalPendingReason {
    fn release_condition(self) -> &'static str {
        match self {
            Self::AwaitingReceipt => "original_send_receipt_available",
            Self::AwaitingCompletion => "origin_completion_available",
            Self::TestRequestDecode => "original_request_readable",
            Self::RowIdentityDecode => "original_row_identity_readable",
            Self::ReceiptDecode => "original_send_receipt_readable",
            Self::SelectorValidate => "original_selector_valid",
            Self::ScopeDecode => "original_sender_scope_readable",
            Self::PeerDecode => "original_peer_custody_readable",
            Self::SettlementDecodeOrValidate => "original_settlement_valid",
            Self::OriginalCustodyChanged => "original_custody_matches",
            Self::CurrentnessCheckFailed | Self::PostReadCurrentnessCheckFailed => {
                "original_custody_read_succeeds"
            }
            Self::RunnerUnavailable => "origin_peer_available",
            Self::LeaseUnavailable => "origin_lease_current",
            Self::RouteChanged | Self::PostReadRouteChanged => "original_route_current",
            Self::OriginTimeout | Self::OriginReadFailed => "origin_read_succeeds",
            Self::CompletionValidate => "origin_completion_valid",
            Self::CompletionNotTerminal => "origin_completion_terminal",
            Self::CompletionOriginMismatch => "completion_matches_original_runner",
            Self::CompletionRunMismatch => "completion_matches_requested_run",
            Self::CompletionInputUnsupported => "completion_has_supported_input_identity",
            Self::CompletionModeMismatch => "completion_matches_requested_mode",
            Self::AdmissionPending => "original_admission_settles",
            Self::AdmissionFailed => "original_settlement_read_and_save_succeed",
        }
    }

    fn custody_failure(self) -> bool {
        matches!(
            self,
            Self::TestRequestDecode
                | Self::RowIdentityDecode
                | Self::ReceiptDecode
                | Self::SelectorValidate
                | Self::ScopeDecode
                | Self::PeerDecode
                | Self::SettlementDecodeOrValidate
        )
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CausalPendingState {
    schema_version: u8,
    reason: CausalPendingReason,
    attempts: u64,
    first_pending_at: String,
    last_attempt_at: Option<String>,
    migrated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_custody_digest: Option<String>,
}

impl CausalPendingState {
    fn new(reason: CausalPendingReason, migrated: bool) -> Self {
        Self {
            schema_version: 1,
            reason,
            attempts: 0,
            first_pending_at: causal_pending_now(),
            last_attempt_at: None,
            migrated,
            legacy_custody_digest: None,
        }
    }

    fn decode(encoded: &str) -> Result<Self> {
        if encoded.len() > 1024 {
            bail!("causal pending metadata exceeds its bound");
        }
        let state: Self = serde_json::from_str(encoded)?;
        if state.schema_version != 1
            || state.legacy_custody_digest.as_ref().is_some_and(|digest| {
                !state.migrated
                    || digest.len() != 64
                    || !digest
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            || chrono::DateTime::parse_from_rfc3339(&state.first_pending_at).is_err()
            || state
                .last_attempt_at
                .as_ref()
                .is_some_and(|at| chrono::DateTime::parse_from_rfc3339(at).is_err())
        {
            bail!("invalid causal pending metadata");
        }
        Ok(state)
    }

    fn diagnostic(&self) -> Value {
        // Private quarantine fingerprints never enter the public diagnostic.
        json!({"schemaVersion":self.schema_version,"reason":self.reason,
            "releaseCondition":self.reason.release_condition(),
            "attempts":self.attempts,"firstPendingAt":self.first_pending_at,
            "lastAttemptAt":self.last_attempt_at,"migrated":self.migrated})
    }
}

enum CausalPendingMetadata {
    Retired,
    Readable(CausalPendingState),
    Malformed,
}

impl CausalPendingMetadata {
    fn from_column(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<Self> {
        // Field contents may be damaged while a store is open. Query/column
        // shape errors still propagate; only this field's bytes are isolated.
        Ok(match row.get_ref(column)? {
            rusqlite::types::ValueRef::Null => Self::Retired,
            rusqlite::types::ValueRef::Text(bytes) => std::str::from_utf8(bytes)
                .ok()
                .and_then(|encoded| CausalPendingState::decode(encoded).ok())
                .map(Self::Readable)
                .unwrap_or(Self::Malformed),
            _ => Self::Malformed,
        })
    }

    fn diagnostic(self) -> Option<Value> {
        match self {
            Self::Retired => None,
            Self::Readable(state) => Some(state.diagnostic()),
            Self::Malformed => Some(json!({
                "schemaVersion":null,"reason":CAUSAL_PENDING_METADATA_REASON,
                "releaseCondition":CAUSAL_PENDING_METADATA_RELEASE,
                "attempts":null,"firstPendingAt":null,"lastAttemptAt":null,"migrated":null
            })),
        }
    }
}

fn causal_pending_order(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<Option<i64>> {
    Ok(match row.get_ref(column)? {
        rusqlite::types::ValueRef::Integer(order) if order > 0 => Some(order),
        _ => None,
    })
}

#[derive(Clone, Copy)]
struct CausalPendingRow {
    intents: bool,
    row_id: i64,
}

impl CausalPendingRow {
    fn table(self) -> &'static str {
        if self.intents {
            "mailbox_relay_intents"
        } else {
            "mailbox_messages"
        }
    }
}

// The partial indexes cover ONLY explicit pending membership. MAX never walks
// ordinary mail. The caller holds the same IMMEDIATE transaction as its send,
// retry or settlement, so retry order cannot race another writer.
fn next_causal_pending_order(connection: &rusqlite::Connection) -> Result<i64> {
    let last = connection.query_row(
        "SELECT MAX(n) FROM (
         SELECT MAX(test_request_pending_order) AS n FROM mailbox_messages
         WHERE test_request_pending_json IS NOT NULL AND test_request_pending_order BETWEEN 1 AND 9223372036854775807
         UNION ALL SELECT MAX(test_request_pending_order) FROM mailbox_relay_intents
         WHERE test_request_pending_json IS NOT NULL AND test_request_pending_order BETWEEN 1 AND 9223372036854775807)",
        [], |row| Ok(match row.get_ref(0)? {
            rusqlite::types::ValueRef::Null => 0,
            rusqlite::types::ValueRef::Integer(order) => order,
            // A damaged REAL order must not poison integer scheduling. The
            // indexed numeric range bounds it; the next integer sorts after it.
            rusqlite::types::ValueRef::Real(order) if order.is_finite() && order >= 1.0
                && order < i64::MAX as f64 => order as i64,
            _ => return Err(rusqlite::Error::InvalidQuery),
        }),
    )?;
    last.checked_add(1)
        .ok_or_else(|| anyhow!("causal pending retry order exhausted"))
}

fn rotate_causal_pending(connection: &rusqlite::Connection, row: CausalPendingRow) -> Result<()> {
    let order = next_causal_pending_order(connection)?;
    connection.execute(&format!(
        "UPDATE {} SET test_request_pending_order=?1 WHERE rowid=?2 AND test_request_pending_json IS NOT NULL",
        row.table()), rusqlite::params![order,row.row_id])?;
    Ok(())
}

fn create_causal_pending(
    connection: &rusqlite::Connection,
    intents: bool,
    owner: &str,
    key: &str,
) -> Result<()> {
    let row = CausalPendingRow { intents, row_id: 0 };
    let owner_column = if intents {
        "owner_session_id"
    } else {
        "sender_session_id"
    };
    let order = next_causal_pending_order(connection)?;
    let state = CausalPendingState::new(
        if intents {
            CausalPendingReason::AwaitingReceipt
        } else {
            CausalPendingReason::AwaitingCompletion
        },
        false,
    );
    let changed = connection.execute(
        &format!(
            "UPDATE {} SET test_request_pending_json=?1,test_request_pending_order=?2
         WHERE {owner_column}=?3 AND idempotency_key=?4 AND test_request_pending_json IS NULL",
            row.table()
        ),
        rusqlite::params![serde_json::to_string(&state)?, order, owner, key],
    )?;
    if changed != 1 {
        bail!("original send missing while recording causal pending membership");
    }
    Ok(())
}

fn note_causal_pending(
    connection: &rusqlite::Connection,
    row: CausalPendingRow,
    reason: CausalPendingReason,
) -> Result<()> {
    let metadata = connection.query_row(
        &format!(
            "SELECT test_request_pending_json FROM {} WHERE rowid=?1",
            row.table()
        ),
        [row.row_id],
        |row| CausalPendingMetadata::from_column(row, 0),
    )?;
    // A final settlement may have retired the key while an origin call ran.
    // A late diagnostic must never resurrect it.
    let mut state = match metadata {
        CausalPendingMetadata::Retired => return Ok(()),
        CausalPendingMetadata::Malformed => {
            // Preserve the evidence and commit progress past this key. Its
            // durable bytes themselves supply the closed diagnostic on read.
            return rotate_causal_pending(connection, row);
        }
        CausalPendingMetadata::Readable(state) => state,
    };
    state.reason = reason;
    if !reason.custody_failure() {
        state.legacy_custody_digest = None;
    }
    state.attempts = state.attempts.saturating_add(1);
    state.last_attempt_at = Some(causal_pending_now());
    let order = next_causal_pending_order(connection)?;
    connection.execute(&format!(
        "UPDATE {} SET test_request_pending_json=?1,test_request_pending_order=?2 WHERE rowid=?3 AND test_request_pending_json IS NOT NULL",
        row.table()), rusqlite::params![serde_json::to_string(&state)?,order,row.row_id])?;
    Ok(())
}

impl MailboxStore {
    fn note_causal_pending_for_selector(
        &self,
        selector: &CausalTestCompletionSelector,
        intents: bool,
        reason: CausalPendingReason,
    ) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // Across I/O, address the original native owner/send key, never an
        // earlier rowid that VACUUM or deletion could have reassigned.
        let table = CausalPendingRow { intents, row_id: 0 }.table();
        let owner = if intents {
            "owner_session_id"
        } else {
            "sender_session_id"
        };
        let row_id: Option<i64> = transaction
            .query_row(
                &format!("SELECT rowid FROM {table} WHERE {owner}=?1 AND idempotency_key=?2"),
                rusqlite::params![
                    selector.payload.requester.native_session_id,
                    selector.payload.request_send_key
                ],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(row_id) = row_id {
            note_causal_pending(&transaction, CausalPendingRow { intents, row_id }, reason)?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn causal_pending_diagnostics(&self) -> Result<Value> {
        let connection = self.connection()?;
        let transaction = connection.unchecked_transaction()?;
        let mut records = Vec::new();
        let mut details = Vec::new();
        let mut detail_bytes_left = CAUSAL_SETTLEMENT_MAX_BYTES;
        let mut admission_details_limited = false;
        let mut total = 0_u64;
        for intents in [false, true] {
            let row = CausalPendingRow { intents, row_id: 0 };
            total += transaction.query_row(
                &format!(
                    "SELECT COUNT(*) FROM {} WHERE test_request_pending_json IS NOT NULL",
                    row.table()
                ),
                [],
                |row| row.get::<_, u64>(0),
            )?;
            let mut statement = transaction.prepare(&format!(
                "SELECT rowid,test_request_pending_json,test_request_pending_order FROM {} WHERE test_request_pending_json IS NOT NULL ORDER BY test_request_pending_order DESC LIMIT ?1",
                row.table()))?;
            let mut rows = statement.query([CAUSAL_PENDING_DIAGNOSTICS as i64])?;
            while let Some(record) = rows.next()? {
                let metadata = CausalPendingMetadata::from_column(record, 1)?;
                let admission_pending = matches!(
                    &metadata,
                    CausalPendingMetadata::Readable(state)
                        if state.reason == CausalPendingReason::AdmissionPending
                );
                let Some(mut state) = metadata.diagnostic() else {
                    continue;
                };
                let row_id = record.get::<_, i64>(0)?;
                if causal_pending_order(record, 2)?.is_none() {
                    state["reason"] = json!(CAUSAL_PENDING_METADATA_REASON);
                    state["releaseCondition"] = json!(CAUSAL_PENDING_METADATA_RELEASE);
                } else if admission_pending && records.len() < CAUSAL_PENDING_DIAGNOSTICS {
                    // Read only returned pending rows, with a byte budget that
                    // can fit any one valid settlement. No scan of retained
                    // completions or allocation for an oversized value.
                    let (present, encoded) = transaction.query_row(
                        &format!(
                            "SELECT test_request_settlement_json IS NOT NULL,
                         CASE WHEN typeof(test_request_settlement_json)='text'
                           AND length(CAST(test_request_settlement_json AS BLOB)) <= ?2
                           THEN test_request_settlement_json END
                         FROM {} WHERE rowid=?1",
                            row.table(),
                        ),
                        rusqlite::params![row_id, detail_bytes_left as i64],
                        |row| {
                            let encoded = match row.get_ref(1)? {
                                rusqlite::types::ValueRef::Text(bytes)
                                    if bytes.len() <= detail_bytes_left =>
                                {
                                    std::str::from_utf8(bytes).ok().map(str::to_owned)
                                }
                                _ => None,
                            };
                            Ok((row.get::<_, bool>(0)?, encoded))
                        },
                    )?;
                    if let Some(encoded) = encoded {
                        detail_bytes_left -= encoded.len();
                        details.push((records.len(), encoded));
                    } else if present {
                        admission_details_limited = true;
                    }
                }
                records.push(json!({"table":row.table(),"rowId":row_id,"state":state}));
            }
        }
        transaction.commit()?;
        drop(connection);
        records.truncate(CAUSAL_PENDING_DIAGNOSTICS);
        for (index, encoded) in details {
            project_causal_admission_checks(&mut records[index]["state"], &encoded);
        }
        Ok(
            json!({"records":records,"total":total,"limited":total > records.len() as u64,
            "admissionDetailsLimited":admission_details_limited,
            "resetOnRestart":false,
            "disposition":"original row retained; targeted authenticated pull only; no completion or receipt inferred",
            "remedy":"report table, rowId, reason and check to the coordinator; do not edit the live store"}),
        )
    }
}
