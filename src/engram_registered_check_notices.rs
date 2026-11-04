// Ordinary-turn notice preparation. Reuses durable launcher registration and
// the original tool's explicit artifact association; never verification authority.
// All native path, artifact and process reads precede the terminal state lock.
struct RegisteredCheckNotice {
    generation: u64,
    runtime: Option<String>,
    check: EngramTurnCheck,
    observation: Option<TestRunCommandObservation>,
    runs: Vec<(TestRunRegistration, Option<TestRunState>)>,
}

impl AppState {
    fn prepare_registered_check_notices(&self, session_id: &str) -> Vec<RegisteredCheckNotice> {
        let mut prepared = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else { return Vec::new() };
            let record = &inner.sessions[index];
            let Some(trace) = record.engram.unadmitted_active_turn.as_ref() else { return Vec::new() };
            let checks = record.engram.active_turn_checks.iter().filter(|check|
                check.end.is_none() && check.host_turn_ref == trace.host_turn_ref
                    && EngramHost::launcher_arguments(&check.command).is_some()
            ).collect::<Vec<_>>();
            if checks.is_empty() { return Vec::new(); }
            let runtime = test_run_runtime_identity(record, &EngramObservationProvenance::Ambient);
            let registrations = inner.test_run_registrations.values().filter(|run|
                run.owning_session_id == session_id && inner.test_runs.registered.contains(&run.run_id))
                .collect::<Vec<_>>();
            checks.into_iter().map(|check| {
                let tool = sha256_hex(check.key.as_bytes());
                let fingerprint = EngramHost::check_fingerprint(&check.command);
                let buckets = [
                    (Some(session_id.to_owned()), Some(sha256_hex(trace.host_turn_ref.as_bytes())), tool.clone()),
                    (Some(session_id.to_owned()), None, tool.clone()),
                ];
                let mut rows = buckets.iter().flat_map(|bucket|
                    inner.test_run_observations.index().check(bucket))
                    .filter_map(|id| inner.test_run_observations.get(id)).filter(|row|
                    row.summary.owner_session_id.as_deref() == Some(session_id)
                        && row.turn_generation == record.active_turn_generation
                        && row.runtime_identity == runtime && runtime.is_some()
                        && !engram_check_key_is_its_line(check)
                        && row.summary.started_at.as_deref() == Some(check.started_at.as_str())
                        && row.tool_identity == tool && row.command_fingerprint == fingerprint);
                let observation = rows.next().cloned().filter(|_| rows.next().is_none());
                let exact_run = observation.as_ref().and_then(|row| row.artifact.as_ref())
                    .map(|artifact| artifact.run_id.as_str());
                let mut runs = registrations.iter().copied().filter(|run|
                    test_run_path_key(&run.worktree)
                            == test_run_path_key(&test_run_display_path(&check.target.root)))
                    .cloned().collect::<Vec<_>>();
                // Selection bounds disclosure/I/O, never proves correlation.
                // Keep an explicitly associated run even amid older history.
                runs.sort_by(|a, b| (Some(b.run_id.as_str()) == exact_run, &b.registered_at)
                    .cmp(&(Some(a.run_id.as_str()) == exact_run, &a.registered_at)));
                runs.truncate(8);
                RegisteredCheckNotice {
                    generation: record.active_turn_generation, runtime: runtime.clone(),
                    check: check.clone(), observation,
                    runs: runs.into_iter().map(|run| (run, None)).collect(),
                }
            }).collect::<Vec<_>>()
        };
        for notice in &mut prepared {
            let associated_run = notice.observation.as_ref().and_then(|row| row.artifact.as_ref())
                .map(|artifact| artifact.run_id.as_str());
            let terminal_digest = notice.observation.as_ref()
                .and_then(|row| row.artifact.as_ref())
                .and_then(|artifact| artifact.terminal_digest.as_deref());
            for (registration, state) in &mut notice.runs {
                if associated_run != Some(registration.run_id.as_str()) { continue; }
                *state = TestRunDisk::read(FsPath::new(&registration.run_directory), None)
                    .filter(|disk| test_run_registered_disk_matches(registration, disk))
                    .filter(|disk| terminal_digest.is_none_or(|digest|
                        disk.results.as_ref().is_some_and(|results|
                            test_run_terminal_digest_matches(Some(&results.digest), digest))))
                    .map(|disk| disk.state_at(&test_run_process_writer_may_be_alive, chrono::Utc::now()));
            }
        }
        prepared
    }
}

fn registered_check_notices_locked(
    inner: &mut StateInner, index: usize, prepared: &[RegisteredCheckNotice],
) -> (Vec<String>, Vec<(String, usize)>) {
    let record = &inner.sessions[index];
    let runtime = test_run_runtime_identity(record, &EngramObservationProvenance::Ambient);
    let mut notices = Vec::new();
    let mut correlated = Vec::new();
    for notice in prepared {
        if record.active_turn_generation != notice.generation || runtime != notice.runtime
            || !record.engram.active_turn_checks.iter().any(|check|
                check.end.is_none() && check.host_turn_ref == notice.check.host_turn_ref
                    && check.sequence == notice.check.sequence && check.key == notice.check.key
                    && check.started_at == notice.check.started_at && check.command == notice.check.command
                    && check.target.root == notice.check.target.root)
        { continue; }
        let rows = notice.runs.iter().filter(|(run, _)|
            inner.test_runs.registered.contains(&run.run_id)
                && inner.test_run_registrations.get(&run.run_id) == Some(run)).collect::<Vec<_>>();
        let exact = notice.observation.as_ref().filter(|row|
            inner.test_run_observations.get(&row.execution_id) == Some(*row))
            .and_then(|row| row.artifact.as_ref()).and_then(|artifact| {
                rows.iter().copied().find(|(run, _)|
                    artifact.run_id == run.run_id
                        && artifact.request_identity.as_ref() == Some(&run.request_identity)
                        && test_run_path_key(&artifact.run_directory) == test_run_path_key(&run.run_directory))
            });
        if let Some((run, Some(state @ (TestRunState::Running | TestRunState::Passed | TestRunState::Failed)))) = exact {
            let status = match state {
                TestRunState::Passed => "passed; consult its terminal result record",
                TestRunState::Failed => "failed; consult its terminal result record",
                _ => "still running; its outcome is pending",
            };
            notices.push(format!("{ENGRAM_CHECK_CREDIT_LINE_PREFIX} registered launcher run {} is {status}. \
                It is correlated with this command's explicit launcher locator. Native verification \
                uses its own run record; do not start a duplicate run.", run.run_id));
            correlated.push((notice.check.host_turn_ref.clone(), notice.check.sequence));
        } else if !rows.is_empty() {
            let ids = rows.iter().map(|(run, _)| run.run_id.as_str()).collect::<Vec<_>>().join(", ");
            notices.push(format!("{ENGRAM_CHECK_CREDIT_LINE_PREFIX} check registered runs {ids} \
                for this root and input before starting another run. Their relationship to this \
                command is unconfirmed; rerun only if none covers the intended check."));
        }
    }
    // The caller preserves native running/carry custody before excluding
    // only leftover correlated checks from Unknown closure.
    (notices, correlated)
}
