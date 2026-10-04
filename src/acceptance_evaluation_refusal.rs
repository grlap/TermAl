// Confirmed no-record ownership. Unknown writes retain the original submission
// ladder; a later refusal never establishes that an earlier send did not land.
impl AppState {
    fn record_acceptance_evaluation_refusal(
        &self,
        authority: &AcceptanceEvaluationSubmitAuthority,
        code: Option<&str>,
        message: &str,
        original: AcceptanceEvaluationOpenWrite,
    ) -> Result<(), ApiError> {
        let (written, dispatch) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index =
                acceptance_evaluation_attempt_index_locked(&inner, authority).ok_or_else(|| {
                    ApiError::conflict("the refused attempt no longer belongs to this delegation")
                })?;
            let target = inner.delegations[index]
                .acceptance_evaluation
                .as_mut()
                .unwrap();
            let history =
                target
                    .attempt_history
                    .get_or_insert_with(|| AcceptanceEvaluationAttemptHistory {
                        // No legacy record becomes eligible merely because it refused.
                        schema_version: 0,
                        parent_session_id: String::new(),
                        child_session_id: String::new(),
                        cwd: String::new(),
                        identity: AcceptanceEvidenceIdentity::default(),
                        ordinal: 0,
                        requester_text_tainted: true,
                        identity_refused: false,
                        previous: Vec::new(),
                        refusal: None,
                        prepared_brief: None,
                    });
            history.refusal = Some(AcceptanceEvaluationDefinitiveRefusal {
                code: code.unwrap_or("usage_error").to_owned(),
                message: message.to_owned(),
                original,
            });
            history.identity_refused |= code.is_some() && message.contains("independent_session evaluation must come from a session that neither holds nor executes the run");
            target.submission = AcceptanceEvaluationSubmission::None;
            inner.mark_delegation_mutated(index);
            let (_, dispatch) = self
                .commit_locked_with_persist_dispatch(&mut inner)
                .map_err(|e| {
                    ApiError::internal(format!(
                        "confirmed refusal retained; its local acknowledgement is unknown: {e:#}"
                    ))
                })?;
            (inner.delegations[index].clone(), dispatch)
        };
        if dispatch != PersistDispatch::Synchronous {
            self.confirm_acceptance_evaluation_submission_durable(authority, &written)
                .map_err(|e| {
                    ApiError::internal(format!(
                        "confirmed refusal retained; its local acknowledgement is unknown: {e}"
                    ))
                })?;
        }
        Ok(())
    }
}
