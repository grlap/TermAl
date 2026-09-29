// The bridge side of `termal_review_freeze_check`: the handler that forwards a
// read-only reviewer's call to `POST /api/sessions/{id}/delegation-review-freeze`
// as a long call (`DelegationLongCall::ReviewFreeze`). Does not own the check
// itself or its budgets (`review_freeze_api.rs`, `review_freeze_process.rs`),
// the allowance and the POST (`delegation_mcp_timeouts.rs`), or the bridge's
// dispatch, caller gate and tool list (`delegation_mcp.rs`). New module: the
// call used to go through the ordinary `post_json` in `delegation_mcp.rs`,
// which is already past its size limit; it now waits the doubled freeze
// budget.

impl TermalDelegationMcpBridge {
    fn tool_review_freeze_check(&self, arguments: Value) -> Result<Value> {
        let path = format!(
            "/api/sessions/{}/delegation-review-freeze",
            self.serving_session_id
        );
        self.post_long_call(DelegationLongCall::ReviewFreeze, &path, &arguments)
    }
}
