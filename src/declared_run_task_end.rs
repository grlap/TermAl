// Ordinary declared task END, beside the native reservation owner. New file.
// Reuses the original observation's start binding and the existing artifact
// reader. Owns no verification reservation, source capture or tracker credit.

impl AppState {
    fn end_observed_declared_task(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        key: &str,
        interrupted: bool,
    ) {
        let provenance = EngramObservationProvenance::Claude(ClaudeObservationProvenance {
            token: token.clone(),
            origin: ClaudeWorkOrigin::Unattributed,
        });
        let original = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            let record = &inner.sessions[index];
            if !record.runtime.matches_runtime_token(token) {
                return;
            }
            let runtime = test_run_runtime_identity(record, &provenance);
            let tool = sha256_hex(key.as_bytes());
            let mut matching = inner.test_run_observations.values().filter(|row| {
                row.summary.owner_session_id.as_deref() == Some(session_id)
                    && row.runtime_identity == runtime
                    && row.tool_identity == tool
                    && row.summary.ended_at.is_none()
            });
            let (Some(row), None) = (matching.next(), matching.next()) else {
                return;
            };
            if row.declared_binding.is_none() {
                return;
            }
            row.clone()
        };
        // Freeze at the original task's END, outside StateInner, before the
        // generic no-exit fallback or a later tool can replace the artifact.
        // The reader checks declaration/path/start identity. A task notice
        // supplies no usable exit, so an ordinary row cannot claim PASS.
        let result = engram_declared_artifact_result(original.declared_binding.as_ref().unwrap());
        let (state, error) = if interrupted {
            (
                TestRunState::Unknown,
                Some("The process was interrupted before its artifact END was frozen.".to_owned()),
            )
        } else {
            match result.verdict {
                EngramTrxVerdict::Pass => (
                    TestRunState::Unknown,
                    Some("The declared command ended without a usable exit status; a passing artifact alone cannot pass an ordinary observed check.".to_owned()),
                ),
                EngramTrxVerdict::Fail(reason) => (TestRunState::Failed, Some(reason)),
                EngramTrxVerdict::Unknown(reason) => (TestRunState::Unknown, Some(reason)),
            }
        };
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        if !inner.sessions[index].runtime.matches_runtime_token(token) {
            return;
        }
        // The read may not settle a replacement execution or a row another
        // completion already closed. This is the captured row, not a new bind.
        let Some(row) = inner.test_run_observations.get(&original.execution_id) else {
            return;
        };
        if row != &original {
            return;
        }
        let ended = self.observe_test_command_end_locked(
            &mut inner,
            session_id,
            &provenance,
            key,
            None,
            None,
        );
        if ended.len() != 1 || ended[0] != original.execution_id {
            return;
        }
        let persist = inner
            .test_run_observations
            .update(&original.execution_id, |row| {
                row.summary.state = state;
                row.summary.error = error;
                row.summary.interrupted = interrupted;
                row.clone()
            });
        if let Some(observation) = persist {
            self.persist_test_command_observation_locked(&mut inner, observation);
        }
    }
}
