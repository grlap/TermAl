// Exact original-key reads for completion-ready scheduling. Reuses the pending
// decoder from test_run_causal_completion_scan.rs; owns no new custody or rows.

use super::*;
pub(crate) enum CausalReadyPlan {
    Ready(CausalTestCompletionPlan),
    Ignored(&'static str),
}

impl MailboxStore {
    pub(crate) fn causal_test_completion_plan_for_key(
        &self,
        requester: &MailboxRelayPrincipal,
        key: &TestRunCausalKey,
    ) -> Result<CausalReadyPlan> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let local_store = mailbox_relay_store_id(&transaction)?;
        // Inspect both original owners, including retired rows: a second owner
        // is ambiguity, never permission to select a convenient pending copy.
        let identities = {
            let mut statement = transaction.prepare(
                "SELECT rowid,0 FROM mailbox_messages WHERE sender_session_id=?1 AND idempotency_key=?2
                 UNION ALL SELECT rowid,1 FROM mailbox_relay_intents WHERE owner_session_id=?1 AND idempotency_key=?2 LIMIT 2",
            )?;
            let identities = statement
                .query_map(
                    rusqlite::params![requester.native_session_id, key.1],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?)),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            identities
        };
        let [(row_id, intents)] = identities.as_slice() else {
            return Ok(CausalReadyPlan::Ignored(
                "original_key_missing_or_ambiguous",
            ));
        };
        let pending = CausalPendingRow {
            intents: *intents,
            row_id: *row_id,
        };
        let table = pending.table();
        let owner = if *intents {
            "owner_session_id"
        } else {
            "sender_session_id"
        };
        let canonical = if *intents { "receipt_json" } else { "id" };
        let peer = if *intents { "peer_json" } else { "NULL" };
        let mut statement = transaction.prepare(&format!(
            "SELECT rowid,{owner},idempotency_key,test_request_json,test_request_scope_json,{canonical},{peer},test_request_settlement_json,test_request_pending_json FROM {table} WHERE rowid=?1",
        ))?;
        let mut rows = statement.query([row_id])?;
        let row = rows
            .next()?
            .ok_or_else(|| anyhow!("original pending row disappeared"))?;
        let decoded = causal_test_completion_plan_from_row(row, *intents, &local_store);
        if matches!(&decoded, Ok(CausalPendingRead::Retired)) {
            return Ok(CausalReadyPlan::Ignored("original_already_settled"));
        }
        if !matches!(
            CausalPendingMetadata::from_column(row, 8)?,
            CausalPendingMetadata::Readable(_)
        ) {
            return Ok(CausalReadyPlan::Ignored(
                "original_pending_membership_unavailable",
            ));
        }
        Ok(match decoded {
            Ok(CausalPendingRead::Ready(plan)) if plan.selector.key() == *key => {
                CausalReadyPlan::Ready(plan)
            }
            Ok(CausalPendingRead::Retired) => CausalReadyPlan::Ignored("original_already_settled"),
            _ => CausalReadyPlan::Ignored("original_pending_custody_unavailable"),
        })
    }
}
