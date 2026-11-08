// Foreground declared observations without an Engram trace. New file.
// Reuses the original row's binding and the ordinary exit-aware judge;
// owns no native reservation, registration or verification authority.

impl AppState {
    fn end_untraced_declared_observation(
        &self,
        session_id: &str,
        provenance: &EngramObservationProvenance,
        key: &str,
        reported_command: Option<&str>,
        exit: Option<EngramCommandExit>,
        #[cfg(test)] after_observation_end: impl FnOnce(),
    ) -> Option<Vec<String>> {
        if exit == Some(EngramCommandExit::NotFinished) {
            return None;
        }
        let (ended, binding) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(session_id)?;
            let record = &inner.sessions[index];
            if record.engram.unadmitted_active_turn.is_some() {
                return None;
            }
            let runtime = test_run_runtime_identity(record, provenance);
            let tool = sha256_hex(key.as_bytes());
            let turn = match provenance {
                EngramObservationProvenance::Runtime {
                    turn_generation, ..
                } => Some(*turn_generation),
                EngramObservationProvenance::Claude(origin) => match &origin.origin {
                    ClaudeWorkOrigin::Attempt { turn_generation } => Some(*turn_generation),
                    ClaudeWorkOrigin::Adopted { .. } | ClaudeWorkOrigin::Unattributed => None,
                },
                EngramObservationProvenance::Ambient => None,
            };
            let rows = inner
                .test_run_observations
                .values()
                .filter(|row| {
                    row.summary.owner_session_id.as_deref() == Some(session_id)
                        && row.runtime_identity == runtime
                        && row.tool_identity == tool
                        && turn.is_none_or(|turn| row.turn_generation == turn)
                        && row.summary.ended_at.is_none()
                })
                .collect::<Vec<_>>();
            if !rows.iter().any(|row| row.declared_artifact.is_some())
                || rows
                    .iter()
                    .any(|row| row.launch_only || row.declared_native.is_some())
            {
                return None;
            }
            let binding = match rows.as_slice() {
                [row] => row.declared_binding.clone().filter(|binding| {
                    row.declared_artifact.as_ref().is_some_and(|path| {
                        test_run_path_key(path) == test_run_path_key(&binding.artifact_canonical)
                    })
                }),
                _ => None,
            };
            // Capture the owner and persist pending in the same END update.
            // A wait must never consume the provisional exit-derived state.
            let ended = self.observe_test_command_end_locked_with_declared_phase(
                &mut inner,
                session_id,
                provenance,
                key,
                exit,
                Some("declared foreground completion has no unique original artifact binding"),
                true,
            );
            (ended, binding)
        };
        #[cfg(test)]
        after_observation_end();
        if ended.len() == 1 {
            let (outcome, detail) = match binding {
                Some(binding) => {
                    let matches = reported_command
                        .and_then(engram_declared_candidate)
                        .is_some_and(|(_, words)| {
                            engram_declared_words(&binding.entry.command)
                                .is_ok_and(|expected| expected == words)
                        });
                    if matches {
                        // File and Git reads remain outside StateInner. A
                        // provider success alone cannot pass this declaration.
                        let exit = exit.unwrap_or(EngramCommandExit::Unknown);
                        let result = engram_declared_result(&binding, exit);
                        let detail = match &result.verdict {
                            EngramTrxVerdict::Unknown(reason) => Some(reason.clone()),
                            _ => None,
                        };
                        (engram_declared_outcome(exit, &result), detail)
                    } else {
                        (Some(EngramExecutionOutcome::Unknown), Some(
                            "declared foreground completion does not match its original command".to_owned(),
                        ))
                    }
                }
                None => (
                    Some(EngramExecutionOutcome::Unknown),
                    Some(
                        "declared foreground completion has no retained original artifact binding"
                            .to_owned(),
                    ),
                ),
            };
            self.apply_declared_verdict_to_test_run_with_detail(&ended, outcome, detail.as_deref());
        }
        Some(ended)
    }
}
