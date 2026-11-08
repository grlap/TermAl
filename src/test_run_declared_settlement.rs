// Producer settlement phase for declared observations. New file.
// Artifact readers retain verdict ownership; waits only observe whether the
// original producer has applied that verdict. No verification authority here.

// A late event may name a previous turn. Reuse only an original binding still
// retained for this exact observation; current declaration files cannot supply it.
fn test_run_late_declared_binding(
    record: &SessionRecord,
    row: &TestRunCommandObservation,
    key: &str,
) -> Option<EngramDeclaredBinding> {
    let grant = row.grant_identity.as_deref()?;
    let mut bindings = Vec::new();
    if let Some(trace) = record.engram.unadmitted_active_turn.as_ref()
        && trace.generation == row.turn_generation
        && sha256_hex(trace.host_turn_ref.as_bytes()) == grant
    {
        if let Some(entry) = trace
            .commands
            .iter()
            .rev()
            .find(|entry| entry.tool_key == key)
            && entry.recognized_command.as_ref().is_some_and(|command| {
                EngramHost::check_fingerprint(command) == row.command_fingerprint
            })
            && let Some(binding) = &entry.declared
        {
            bindings.push(binding.clone());
        }
    }
    let runtime = record.runtime.runtime_token();
    for check in record.engram.active_turn_checks.iter().chain(
        record
            .engram
            .carried_checks
            .iter()
            .filter(|carried| carried.ends_by_report && carried.runtime == runtime)
            .map(|carried| &carried.check),
    ) {
        if check.key == key
            && check.end.is_none()
            && sha256_hex(check.host_turn_ref.as_bytes()) == grant
            && EngramHost::check_fingerprint(&check.command) == row.command_fingerprint
            && test_run_path_key(&test_run_display_path(&check.target.root))
                == test_run_path_key(&row.summary.worktree)
            && let Some(binding) = check.declared.as_deref()
        {
            bindings.push(binding.clone());
        }
    }
    bindings.retain(|binding| {
        row.declared_artifact.as_ref().is_some_and(|path| {
            test_run_path_key(path) == test_run_path_key(&binding.artifact_canonical)
        })
    });
    // The trace and diagnostic carry can retain the same original start,
    // regardless of their position in the retained collection.
    let mut unique = Vec::new();
    for binding in bindings {
        if !unique.contains(&binding) {
            unique.push(binding);
        }
    }
    let [binding] = unique.as_slice() else {
        return None;
    };
    Some(binding.clone())
}

impl AppState {
    fn settle_reported_test_observation(
        &self,
        session_id: &str,
        key: &str,
        command: &str,
        exit: EngramCommandExit,
        #[cfg(test)] after_observation_end: impl FnOnce(),
    ) {
        let (ended, declared, binding) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            let record = &inner.sessions[index];
            let runtime = test_run_runtime_identity(record, &EngramObservationProvenance::Ambient);
            let tool = sha256_hex(key.as_bytes());
            let rows = inner
                .test_run_observations
                .values()
                .filter(|row| {
                    row.summary.owner_session_id.as_deref() == Some(session_id)
                        && row.runtime_identity == runtime
                        && row.tool_identity == tool
                        && row.summary.ended_at.is_none()
                })
                .collect::<Vec<_>>();
            let declared = rows.iter().any(|row| row.declared_artifact.is_some());
            let binding = match rows.as_slice() {
                [row] if declared => test_run_late_declared_binding(record, row, key),
                _ => None,
            };
            let ended = self.observe_test_command_end_locked_with_declared_phase(
                &mut inner,
                session_id,
                &EngramObservationProvenance::Ambient,
                key,
                Some(exit),
                None,
                declared,
            );
            (ended, declared, binding)
        };
        #[cfg(test)]
        after_observation_end();
        if !declared || ended.len() != 1 {
            return;
        }
        let (outcome, detail) = if let Some(binding) = binding {
            let matches = engram_declared_candidate(command).is_some_and(|(_, words)| {
                engram_declared_words(&binding.entry.command)
                    .is_ok_and(|expected| expected == words)
            });
            if matches {
                // Ordinary observations use the same exit-aware judge as
                // their carried check; artifact-only judging is native-only.
                let result = engram_declared_result(&binding, exit);
                (engram_declared_outcome(exit, &result), None)
            } else {
                (
                    Some(EngramExecutionOutcome::Unknown),
                    Some("declared late completion does not match its original command"),
                )
            }
        } else {
            (
                Some(EngramExecutionOutcome::Unknown),
                Some("declared late completion has no retained original artifact binding"),
            )
        };
        self.apply_declared_verdict_to_test_run_with_detail(&ended, outcome, detail);
    }

    fn observe_test_command_end_awaiting_declared_verdict(
        &self,
        session_id: &str,
        provenance: &EngramObservationProvenance,
        key: &str,
        exit: Option<EngramCommandExit>,
    ) -> Vec<String> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        // The first persisted END already includes the pending phase. A
        // second update after END would leave a crash window on disk even
        // while StateInner prevented an in-memory wait refresh.
        self.observe_test_command_end_locked_with_declared_phase(
            &mut inner, session_id, provenance, key, exit, None, true,
        )
    }
}
