// Resolved-detail retention beside src/test_runs.rs. New boundary.
// Owns minimal durable identities/proofs and content-fenced compaction;
// does not delete artifacts, mint credit, poll work or introduce an archive.

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct TestRunCompacted {
    compacted: bool,
    request_identity: Option<String>,
    summary: TestRunSummary,
    registration: Option<TestRunRegistration>,
    observations: Vec<TestRunCommandObservation>,
    request_stamp: TestRunFileStamp,
    results_stamp: TestRunFileStamp,
    reason: String,
    #[serde(default)]
    artifact_conflict: bool,
    #[serde(default)]
    artifact_unavailable: bool,
}

#[derive(Clone, Default)]
struct TestRunCompactionPersistence(
    Arc<Mutex<BTreeMap<String, (TestRunCompacted, PersistFenceWaiter)>>>,
);

impl std::fmt::Debug for TestRunCompactionPersistence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TestRunCompactionPersistence")
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunHistory {
    detail_history_limit: usize,
    compacted_count: usize,
    omitted_count: usize,
    unresolved_backlog: usize,
}

fn test_run_minimal_summary(mut summary: TestRunSummary) -> TestRunSummary {
    summary.command = None;
    summary.command_truncated = false;
    summary.current_stage = None;
    summary.stages.clear();
    summary.error = None;
    summary.notify_to = None;
    summary.notify_session_id = None;
    summary.registration.planned_stage_names.clear();
    summary
}

fn test_run_minimal_observation(mut row: TestRunCommandObservation) -> TestRunCommandObservation {
    row.summary = test_run_minimal_summary(row.summary);
    row
}

fn test_run_unavailable_artifact(mut run: TestRunSummary) -> TestRunSummary {
    run.registration.artifact_unavailable = Some(true);
    if matches!(run.state, TestRunState::Running | TestRunState::Unknown) {
        run.state = TestRunState::Unknown;
        run.interrupted = true;
        run.unknown_reason = Some(TestRunUnknownReason::ResultsUnreadable);
        run.current_stage = None;
    }
    run
}

fn test_run_summary_ids(run: &TestRunSummary) -> impl Iterator<Item = &str> {
    std::iter::once(run.run_id.as_str()).chain(
        run.registration
            .superseded_run_ids
            .iter()
            .map(String::as_str),
    )
}

fn test_run_compact_is_installed(inner: &StateInner, row: &TestRunCompacted) -> bool {
    let index = &inner.test_runs;
    let registrations = &inner.test_run_registrations;
    let observations = &inner.test_run_observations;
    (!row.compacted
        || (index.compactions_durable.contains(&row.summary.run_id)
            && index.find(&row.summary.run_id).is_none()))
        && !registrations.contains_key(&row.summary.run_id)
        && row
            .observations
            .iter()
            .all(|observation| !observations.contains_key(&observation.execution_id))
}

fn test_run_compacted_summary(inner: &StateInner, row: &TestRunCompacted) -> TestRunSummary {
    let index = &inner.test_runs;
    let mut summary = row.summary.clone();
    summary.registration.history_compacted = Some(row.compacted);
    summary.registration.artifact_unavailable = if !row.compacted && !index.scanned_once {
        None
    } else {
        Some(row.artifact_unavailable || (!row.compacted && index.find(&summary.run_id).is_none()))
    };
    summary.registration.compaction_reason = Some(row.reason.clone());
    if !row.compacted {
        if summary.registration.artifact_unavailable == Some(true) {
            summary = test_run_unavailable_artifact(summary);
        }
        summary.registration.restart_would_void_evidence = inner
            .sessions
            .iter()
            .flat_map(|record| &record.engram.carried_checks)
            .any(|check| {
                check.run_directory.as_ref().is_some_and(|path| {
                    test_run_path_key(&test_run_display_path(path))
                        == test_run_path_key(&summary.run_dir)
                }) && check.terminal_digest.as_ref().is_some_and(|digest| {
                    test_run_terminal_digest_matches(summary.detail_version.as_deref(), digest)
                })
            })
            .then_some(true);
        return summary;
    }
    let mut unknown = !index.compactions_durable.contains(&summary.run_id)
        || index.compactions_revalidating.contains(&summary.run_id)
        || row.artifact_conflict;
    let mut positive = false;
    for observation in &row.observations {
        let risk = test_run_observation_risk(
            inner,
            observation,
            index.compactions_durable.contains(&summary.run_id),
        );
        positive |= risk == Some(true);
        unknown |= risk.is_none();
    }
    summary.registration.restart_would_void_evidence = if positive {
        Some(true)
    } else if unknown {
        None
    } else {
        Some(false)
    };
    summary.registration.restart_evidence_reason = if positive {
        "Retained exact verification requires settlement or work completion."
    } else if unknown {
        "Retained proof requires current canonical revalidation."
    } else {
        "Durable compact identity retains every resolved execution and verification proof."
    }
    .to_owned();
    summary
}

fn test_run_compaction_eligible(inner: &StateInner, run: &TestRunSummary) -> bool {
    let index = &inner.test_runs;
    if !matches!(run.state, TestRunState::Passed | TestRunState::Failed)
        || run.ended_at.is_none()
        || run.registration.restart_would_void_evidence != Some(false)
    {
        return false;
    }
    let ids: HashSet<_> = test_run_summary_ids(run).collect();
    if inner
        .test_run_waits
        .iter()
        .any(|wait| wait.run_ids.iter().any(|id| ids.contains(id.as_str())))
        || inner
            .test_run_cards
            .get(&run.run_id)
            .is_some_and(|card| !card.terminal)
        || index.registration_failures.contains(&run.run_id)
        || (inner.test_run_registrations.contains_key(&run.run_id)
            && !index.registered.contains(&run.run_id))
    {
        return false;
    }
    let pending = index
        .observation_persistence
        .0
        .lock()
        .expect("test observation mutex poisoned");
    inner
        .test_run_observations
        .values()
        .filter(|row| ids.contains(row.execution_id.as_str()))
        .all(|row| {
            index.observations_durable.contains(&row.execution_id)
                && !index.observations_failed.contains(&row.execution_id)
                && !pending.contains_key(&row.execution_id)
        })
}

fn test_run_compact_artifact_matches(row: &TestRunCompacted, disk: &TestRunDisk) -> bool {
    disk.run_id == row.summary.run_id
        && disk.owner == row.summary.owner_session_id
        && test_run_path_key(&disk.worktree) == test_run_path_key(&row.summary.worktree)
        && test_run_path_key(&test_run_display_path(&disk.run_dir))
            == test_run_path_key(&row.summary.run_dir)
        && disk.results.as_ref().map(|result| &result.digest) == row.summary.detail_version.as_ref()
        && row.registration.as_ref().is_none_or(|registration| {
            disk.request_identity.as_ref() == Some(&registration.request_identity)
        })
        && row.observations.iter().all(|observation| {
            observation.artifact.as_ref().is_none_or(|artifact| {
                artifact
                    .request_identity
                    .as_ref()
                    .is_none_or(|identity| disk.request_identity.as_ref() == Some(identity))
            })
        })
}

fn test_run_history_locked(inner: &StateInner, project: Option<&str>) -> TestRunHistory {
    let included = |summary: &TestRunSummary| {
        project.is_none_or(|id| summary.project_id.as_deref() == Some(id))
    };
    let mut compacted_count = 0;
    let mut omitted_count = 0;
    for row in inner.test_run_compacted.values().filter(|row| {
        row.compacted && test_run_compact_is_installed(inner, row) && included(&row.summary)
    }) {
        compacted_count += 1;
        omitted_count += usize::from(
            test_run_compacted_summary(inner, row)
                .registration
                .restart_would_void_evidence
                == Some(false),
        );
    }
    let unresolved_backlog = test_run_summaries_locked(inner)
        .iter()
        .filter(|run| included(run) && run.registration.restart_would_void_evidence != Some(false))
        .count();
    TestRunHistory {
        detail_history_limit: TEST_RUN_TERMINAL_RUNS_PER_PROJECT,
        compacted_count,
        omitted_count,
        unresolved_backlog,
    }
}

impl AppState {
    fn persist_test_run_compacted_locked(
        &self,
        inner: &mut StateInner,
        row: TestRunCompacted,
    ) -> Result<bool> {
        // This private proof save has no visible revision of its own. The
        // scan or compaction delta publishes the corresponding list change.
        let dispatch = self.persist_internal_locked_with_dispatch(inner)?;
        if matches!(dispatch, PersistDispatch::Synchronous) {
            inner
                .test_runs
                .compactions_durable
                .insert(row.summary.run_id.clone());
            inner
                .test_runs
                .compactions_revalidating
                .remove(&row.summary.run_id);
            Ok(true)
        } else {
            self.queue_test_run_compaction_fence(inner, row);
            Ok(false)
        }
    }
    fn queue_test_run_compaction_fence(&self, inner: &StateInner, row: TestRunCompacted) {
        let pending = &inner.test_runs.compaction_persistence;
        let (fence, waiter) = PersistFence::new(
            PersistFenceTarget::TestRunCompaction(Box::new(row.clone())),
            std::time::Instant::now() + Duration::from_secs(5),
        );
        if self
            .persist_tx
            .send(PersistRequest::Fence(Box::new(fence)))
            .is_ok()
        {
            pending
                .0
                .lock()
                .expect("test compaction mutex poisoned")
                .insert(row.summary.run_id.clone(), (row, waiter));
        }
    }
    fn compact_test_runs_locked(&self, inner: &mut StateInner) {
        // One canonical population, terminal time + stable identity, including
        // the explicit null-project bucket. Unknown obligations are unbounded.
        let mut runs = test_run_full_summaries_locked(inner);
        runs.sort_by(|a, b| {
            b.ended_at
                .cmp(&a.ended_at)
                .then_with(|| a.run_id.cmp(&b.run_id))
        });
        let mut kept = HashMap::<Option<String>, usize>::new();
        for run in runs {
            if !test_run_compaction_eligible(inner, &run) {
                continue;
            }
            let count = kept.entry(run.project_id.clone()).or_default();
            *count += 1;
            if *count <= TEST_RUN_TERMINAL_RUNS_PER_PROJECT {
                continue;
            }
            let pending = inner.test_runs.compaction_persistence.clone();
            if pending
                .0
                .lock()
                .expect("test compaction mutex poisoned")
                .contains_key(&run.run_id)
            {
                continue;
            }
            let ids: HashSet<_> = test_run_summary_ids(&run).collect();
            let observations = inner
                .test_run_observations
                .values()
                .filter(|row| ids.contains(row.execution_id.as_str()))
                .cloned()
                .map(test_run_minimal_observation)
                .collect();
            let disk = inner.test_runs.find(&run.run_id).map(|entry| &entry.disk);
            let compacted = TestRunCompacted {
                compacted: true,
                request_identity: disk.and_then(|disk| disk.request_identity.clone()),
                request_stamp: disk.and_then(|disk| disk.request_stamp),
                results_stamp: disk.and_then(|disk| disk.results_stamp),
                registration: inner.test_run_registrations.get(&run.run_id).cloned().map(
                    |mut registration| {
                        registration.stage_names.clear();
                        registration
                    },
                ),
                observations,
                artifact_unavailable: run.run_dir.is_empty(),
                summary: test_run_minimal_summary(run),
                reason: "Resolved terminal detail exceeds the per-project history limit."
                    .to_owned(),
                artifact_conflict: false,
            };
            let id = compacted.summary.run_id.clone();
            let previous = inner
                .test_run_compacted
                .insert(id.clone(), compacted.clone());
            match self.persist_test_run_compacted_locked(inner, compacted.clone()) {
                Ok(true) => {
                    self.finish_test_run_compaction_locked(inner, &compacted);
                    self.publish_test_run_compact_risk_locked(inner, &compacted);
                }
                Ok(false) => {}
                Err(_) => {
                    if let Some(previous) = previous {
                        inner.test_run_compacted.insert(id, previous);
                    } else {
                        inner.test_run_compacted.remove(&id);
                    }
                }
            }
        }
    }

    fn finish_test_run_compaction_locked(
        &self,
        inner: &mut StateInner,
        expected: &TestRunCompacted,
    ) {
        let id = &expected.summary.run_id;
        if inner.test_run_compacted.get(id) != Some(expected) {
            return;
        }
        let current = test_run_full_summaries_locked(inner)
            .into_iter()
            .find(|run| &run.run_id == id);
        if !current.as_ref().is_some_and(|run| {
            test_run_compaction_eligible(inner, run)
                && test_run_minimal_summary(run.clone()) == expected.summary
        }) {
            return;
        }
        if expected.observations.iter().any(|row| {
            inner
                .test_run_observations
                .get(&row.execution_id)
                .is_none_or(|actual| test_run_minimal_observation(actual.clone()) != *row)
        }) {
            return;
        }
        if expected.registration.as_ref().is_some_and(|row| {
            inner.test_run_registrations.get(id).is_none_or(|actual| {
                let mut actual = actual.clone();
                actual.stage_names.clear();
                actual != *row
            })
        }) {
            return;
        }
        // The exact survivor/proofs are already durable. A subsequent removal
        // commit may fail; then retain all old detail and retry on a later tick.
        let registration = inner.test_run_registrations.remove(id);
        let observations: Vec<_> = expected
            .observations
            .iter()
            .filter_map(|row| inner.test_run_observations.remove(&row.execution_id))
            .collect();
        if self.commit_locked_with_persist_dispatch(inner).is_err() {
            if let Some(registration) = registration {
                inner
                    .test_run_registrations
                    .insert(id.clone(), registration);
            }
            for row in observations {
                inner
                    .test_run_observations
                    .insert(row.execution_id.clone(), row);
            }
            return;
        }
        let index = &mut inner.test_runs;
        index.entries.retain(|entry| &entry.summary.run_id != id);
        index.parsed.retain(|_, disk| &disk.run_id != id);
        index.first_sight.remove(id);
        for row in &expected.observations {
            index.observations_durable.remove(&row.execution_id);
            index.observations_failed.remove(&row.execution_id);
        }
    }

    fn step_test_run_compaction(&self) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let pending = inner.test_runs.compaction_persistence.clone();
        let completed: Vec<_> = pending
            .0
            .lock()
            .expect("test compaction mutex poisoned")
            .iter()
            .filter_map(|(id, (row, waiter))| {
                waiter
                    .wait_until(std::time::Instant::now())
                    .map(|result| (id.clone(), row.clone(), result))
            })
            .collect();
        for (id, row, result) in completed {
            pending
                .0
                .lock()
                .expect("test compaction mutex poisoned")
                .remove(&id);
            if result.is_ok() && inner.test_run_compacted.get(&id) == Some(&row) {
                inner.test_runs.compactions_revalidating.remove(&id);
                inner.test_runs.compactions_durable.insert(id);
                self.finish_test_run_compaction_locked(&mut inner, &row);
                self.publish_test_run_compact_risk_locked(&mut inner, &row);
            }
        }
        self.compact_test_runs_locked(&mut inner);
    }

    fn publish_test_run_compact_risk_locked(&self, inner: &mut StateInner, row: &TestRunCompacted) {
        if !test_run_compact_is_installed(inner, row) {
            return;
        }
        let run = test_run_compacted_summary(inner, row);
        if let Ok(revision) = self.commit_delta_locked(inner) {
            let event = if run.registration.restart_would_void_evidence == Some(false) {
                DeltaEvent::TestRunRemoved {
                    revision,
                    run_id: run.run_id,
                }
            } else {
                DeltaEvent::TestRunChanged { revision, run }
            };
            self.publish_delta_locked(inner, event);
        }
    }

    fn revalidate_test_run_compacted_locked(
        &self,
        inner: &mut StateInner,
        store: &EngramAuthorityStoreKey,
        binding: &EngramControlWorkBinding,
        read: &EngramNamedRootReadResponse,
    ) {
        let mut changed = Vec::new();
        let mut validated = Vec::new();
        for row in inner.test_run_compacted.values_mut() {
            let mut updated = false;
            for observation in &mut row.observations {
                for association in &mut observation.evidence {
                    if &association.store == store
                        && association.binding.root_execution_id == binding.root_execution_id
                        && association.binding.work_id == binding.work_id
                        && association.binding.run_id == binding.run_id
                        && association.binding.claim_id == binding.claim_id
                        && read.latest_event.as_ref().map(|event| event.generation)
                            == association.source_basis.source_root_generation
                    {
                        association.lifecycle_read = Some(read.clone());
                        validated.push((
                            observation.execution_id.clone(),
                            association.producer_observation_id.clone(),
                        ));
                        updated = true;
                    }
                }
            }
            if updated {
                changed.push(row.clone());
            }
        }
        inner.test_runs.lifecycle_revalidated.extend(validated);
        for row in changed {
            let id = row.summary.run_id.clone();
            inner.test_runs.compactions_revalidating.insert(id.clone());
            let _ = self.persist_test_run_compacted_locked(inner, row.clone());
            self.publish_test_run_compact_risk_locked(inner, &row);
        }
    }
}
