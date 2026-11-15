// Row-local scan failures are scheduling diagnostics, never custody or credit.
// Keep bounded metadata in memory; the original row remains untouched.
const CAUSAL_COMPLETION_SCAN_SKIPS: usize = 64;

#[derive(Clone, Copy)]
enum CausalTestCompletionScanFailure {
    Custody,
    Identity,
    Receipt,
    Selector,
    Scope,
    Peer,
    Settlement,
}

impl CausalTestCompletionScanFailure {
    fn reason(self) -> &'static str {
        match self {
            Self::Custody => "test_request_decode",
            Self::Identity => "row_identity_decode",
            Self::Receipt => "receipt_decode",
            Self::Selector => "selector_validate",
            Self::Scope => "scope_decode",
            Self::Peer => "peer_decode",
            Self::Settlement => "settlement_decode_or_validate",
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CausalTestCompletionScanSkip {
    table: &'static str,
    row_id: i64,
    reason: &'static str,
    attempts: u64,
    first_skipped_at: String,
    last_skipped_at: String,
}

#[derive(Default)]
struct CausalTestCompletionScanSkips {
    records: Mutex<(VecDeque<CausalTestCompletionScanSkip>, u64)>,
}

impl CausalTestCompletionScanSkips {
    fn note(&self, intents: bool, row_id: i64, failure: CausalTestCompletionScanFailure) {
        let table = if intents {
            "mailbox_relay_intents"
        } else {
            "mailbox_messages"
        };
        let reason = failure.reason();
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let mut state = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(record) = state
            .0
            .iter_mut()
            .find(|record| record.table == table && record.row_id == row_id)
        {
            record.attempts = record.attempts.saturating_add(1);
            record.reason = reason;
            record.last_skipped_at = now;
        } else {
            if state.0.len() == CAUSAL_COMPLETION_SCAN_SKIPS {
                state.0.pop_front();
                state.1 = state.1.saturating_add(1);
            }
            state.0.push_back(CausalTestCompletionScanSkip {
                table,
                row_id,
                reason,
                attempts: 1,
                first_skipped_at: now.clone(),
                last_skipped_at: now,
            });
        }
    }

    fn snapshot(&self) -> (Vec<CausalTestCompletionScanSkip>, u64) {
        let state = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (state.0.iter().cloned().collect(), state.1)
    }
}

// SQL iteration and the rowid read remain page errors. Once row identity is
// known, malformed custody must not discard healthy plans or pin this page.
fn causal_test_completion_plan_from_row(
    row: &rusqlite::Row<'_>,
    intents: bool,
    local_store: &str,
) -> std::result::Result<Option<CausalTestCompletionPlan>, CausalTestCompletionScanFailure> {
    use CausalTestCompletionScanFailure as Failure;
    let Some(payload) = mailbox_test_payload_from_row(row, 3).map_err(|_| Failure::Custody)? else {
        return Ok(None);
    };
    let owner: String = row.get(1).map_err(|_| Failure::Identity)?;
    let key: String = row.get(2).map_err(|_| Failure::Identity)?;
    // Public relay copies never acquire original sender authority.
    if owner != payload.requester.native_session_id
        || key != payload.request_send_key
        || payload.requester.store_id != local_store
    {
        return Ok(None);
    }
    let stored: Option<String> = row.get(5).map_err(|_| Failure::Receipt)?;
    let Some(stored) = stored else {
        return Ok(None);
    };
    let canonical_message_id = if intents {
        serde_json::from_str::<MailboxAppendReceipt>(&stored)
            .map_err(|_| Failure::Receipt)?
            .message_id
    } else {
        stored
    };
    let selector = CausalTestCompletionSelector {
        canonical_message_id,
        payload,
    };
    selector.validate().map_err(|_| Failure::Selector)?;
    let scope = mailbox_test_scope_from_row(row, 4).map_err(|_| Failure::Scope)?;
    let peer: Option<String> = row.get(6).map_err(|_| Failure::Peer)?;
    let peer = peer
        .map(|json| serde_json::from_str(&json))
        .transpose()
        .map_err(|_| Failure::Peer)?;
    let encoded: Option<String> = row.get(7).map_err(|_| Failure::Settlement)?;
    let settlement = encoded
        .as_deref()
        .map(|encoded| CausalTestSettlement::decode(encoded, &selector, scope.as_ref()))
        .transpose()
        .map_err(|_| Failure::Settlement)?;
    if settlement
        .as_ref()
        .is_some_and(CausalTestSettlement::complete)
    {
        return Ok(None);
    }
    Ok(Some(CausalTestCompletionPlan {
        selector,
        scope,
        peer,
        settlement,
    }))
}
