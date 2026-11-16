// Original-row decoding extracted from the former history sweep. Pending
// membership schedules a lookup; these guards alone decide original authority.
enum CausalPendingRead {
    Ready(CausalTestCompletionPlan),
    Waiting(CausalPendingReason),
    Retired,
    NotOriginal,
}

// SQL iteration and the rowid read remain page errors. Once row identity is
// known, malformed custody must not discard healthy plans or pin this page.
fn causal_test_completion_plan_from_row(
    row: &rusqlite::Row<'_>,
    intents: bool,
    local_store: &str,
) -> std::result::Result<CausalPendingRead, CausalPendingReason> {
    use CausalPendingReason as Failure;
    let Some(payload) =
        mailbox_test_payload_from_row(row, 3).map_err(|_| Failure::TestRequestDecode)?
    else {
        if row
            .get::<_, Option<String>>(4)
            .map_err(|_| Failure::ScopeDecode)?
            .is_some()
            || row
                .get::<_, Option<String>>(7)
                .map_err(|_| Failure::SettlementDecodeOrValidate)?
                .is_some()
        {
            return Err(Failure::TestRequestDecode);
        }
        return Ok(CausalPendingRead::NotOriginal);
    };
    let owner: String = row.get(1).map_err(|_| Failure::RowIdentityDecode)?;
    let key: String = row.get(2).map_err(|_| Failure::RowIdentityDecode)?;
    // Public relay copies never acquire original sender authority.
    if owner != payload.requester.native_session_id
        || key != payload.request_send_key
        || payload.requester.store_id != local_store
    {
        return Ok(CausalPendingRead::NotOriginal);
    }
    let scope = mailbox_test_scope_from_row(row, 4).map_err(|_| Failure::ScopeDecode)?;
    if scope.as_ref().is_some_and(|scope| {
        scope.project_id.is_empty()
            || scope.store.project_id.is_empty()
            || scope.store.database_path.as_os_str().is_empty()
    }) {
        return Err(Failure::ScopeDecode);
    }
    let peer: Option<String> = row.get(6).map_err(|_| Failure::PeerDecode)?;
    let peer = peer
        .map(|json| serde_json::from_str(&json))
        .transpose()
        .map_err(|_| Failure::PeerDecode)?;
    let stored: Option<String> = row.get(5).map_err(|_| Failure::ReceiptDecode)?;
    // The original send owner retains an unconfirmed send. Never guess its id.
    let Some(stored) = stored else {
        if row
            .get::<_, Option<String>>(7)
            .map_err(|_| Failure::SettlementDecodeOrValidate)?
            .is_some()
        {
            return Err(Failure::SettlementDecodeOrValidate);
        }
        return Ok(CausalPendingRead::Waiting(Failure::AwaitingReceipt));
    };
    let canonical_message_id = if intents {
        serde_json::from_str::<MailboxAppendReceipt>(&stored)
            .map_err(|_| Failure::ReceiptDecode)?
            .message_id
    } else {
        stored
    };
    let selector = CausalTestCompletionSelector {
        canonical_message_id,
        payload,
    };
    selector.validate().map_err(|_| Failure::SelectorValidate)?;
    let encoded: Option<String> = row
        .get(7)
        .map_err(|_| Failure::SettlementDecodeOrValidate)?;
    let settlement = encoded
        .as_deref()
        .map(|encoded| CausalTestSettlement::decode(encoded, &selector, scope.as_ref()))
        .transpose()
        .map_err(|_| Failure::SettlementDecodeOrValidate)?;
    // A final receipt/refusal owns completion; no further origin RPC is needed.
    if settlement
        .as_ref()
        .is_some_and(CausalTestSettlement::complete)
    {
        return Ok(CausalPendingRead::Retired);
    }
    Ok(CausalPendingRead::Ready(CausalTestCompletionPlan {
        selector,
        scope,
        peer,
        settlement,
        pending: CausalPendingRow {
            intents,
            row_id: row.get(0).map_err(|_| Failure::RowIdentityDecode)?,
        },
    }))
}
