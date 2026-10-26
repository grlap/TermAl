// Immutable inputs for the existing test-run projection. A scan materializes
// summaries off-lock, then validates these exact inputs at its commit cut.
// This is disposable work, not a cache; unstamped mutations are compared too.
// Included by test_run_observations.rs beside its borrowed projection owner.
// Session bodies, runtime state and persistence waiters are not summary inputs.

struct TestRunProjectionSession {
    id: String,
    name: String,
    hidden: bool,
    child: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct TestRunProjectionSessionRef<'a> {
    id: &'a str,
    name: &'a str,
    hidden: bool,
    child: bool,
}

struct TestRunProjectionCarried {
    run_directory: Option<PathBuf>,
    terminal_digest: Option<String>,
    terminal_conflict: bool,
    ambiguous: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct TestRunProjectionCarriedRef<'a> {
    run_directory: Option<&'a FsPath>,
    terminal_digest: Option<&'a str>,
    terminal_conflict: bool,
    ambiguous: bool,
}

// Both the live model and an owned cut use the same proof/summary functions.
// Keeping their readable surface explicit makes new summary inputs an audit.
trait TestRunProjectionSource {
    fn projection_index(&self) -> &TestRunIndex;
    fn projection_registrations(&self) -> &BTreeMap<String, TestRunRegistration>;
    fn projection_observations(&self) -> &BTreeMap<String, TestRunCommandObservation>;
    fn projection_compacted(&self) -> &BTreeMap<String, TestRunCompacted>;
    fn projection_project_exists(&self, id: &str) -> bool;
    fn projection_sessions(&self) -> impl Iterator<Item = TestRunProjectionSessionRef<'_>>;
    fn projection_carried(&self) -> impl Iterator<Item = TestRunProjectionCarriedRef<'_>>;
    fn projection_session_by_id(&self, id: &str) -> Option<TestRunProjectionSessionRef<'_>> {
        self.projection_sessions().find(|row| row.id == id)
    }
    fn projection_has_observation(&self, id: &str) -> bool {
        self.projection_observations().contains_key(id)
    }
}

impl TestRunProjectionSource for StateInner {
    fn projection_index(&self) -> &TestRunIndex {
        &self.test_runs
    }
    fn projection_registrations(&self) -> &BTreeMap<String, TestRunRegistration> {
        &self.test_run_registrations
    }
    fn projection_observations(&self) -> &BTreeMap<String, TestRunCommandObservation> {
        &self.test_run_observations
    }
    fn projection_compacted(&self) -> &BTreeMap<String, TestRunCompacted> {
        &self.test_run_compacted
    }
    fn projection_project_exists(&self, id: &str) -> bool {
        self.projects.iter().any(|project| project.id == id)
    }
    fn projection_session_by_id(&self, id: &str) -> Option<TestRunProjectionSessionRef<'_>> {
        let record = &self.sessions[self.find_session_index(id)?];
        // Child status has no role in exact-id resolution.
        Some(TestRunProjectionSessionRef {
            id: &record.session.id,
            name: &record.session.name,
            hidden: record.hidden,
            child: false,
        })
    }
    fn projection_sessions(&self) -> impl Iterator<Item = TestRunProjectionSessionRef<'_>> {
        let children: HashSet<_> = self
            .delegations
            .iter()
            .map(|row| row.child_session_id.as_str())
            .collect();
        self.sessions
            .iter()
            .map(move |record| TestRunProjectionSessionRef {
                id: &record.session.id,
                name: &record.session.name,
                hidden: record.hidden,
                child: children.contains(record.session.id.as_str()),
            })
    }
    fn projection_carried(&self) -> impl Iterator<Item = TestRunProjectionCarriedRef<'_>> {
        self.sessions
            .iter()
            .flat_map(|record| &record.engram.carried_checks)
            .map(|check| TestRunProjectionCarriedRef {
                run_directory: check.run_directory.as_deref(),
                terminal_digest: check.terminal_digest.as_deref(),
                terminal_conflict: check.terminal_conflict,
                ambiguous: check.ambiguous,
            })
    }
}

fn test_run_projection_carried_matches(
    source: &impl TestRunProjectionSource,
    directory: &FsPath,
    digest: Option<&str>,
    require_unambiguous: bool,
) -> bool {
    source.projection_carried().any(|check| {
        check.run_directory.is_some_and(|path| {
            test_run_path_key(&test_run_display_path(path))
                == test_run_path_key(&test_run_display_path(directory))
        }) && check
            .terminal_digest
            .is_some_and(|terminal| test_run_terminal_digest_matches(digest, terminal))
            && (!require_unambiguous || (!check.terminal_conflict && !check.ambiguous))
    })
}

fn test_run_project_entries(
    source: &impl TestRunProjectionSource,
    scanned: &[TestRunScanned],
) -> Vec<TestRunEntry> {
    let projection = TestRunProjection::new(source);
    scanned
        .iter()
        .map(|run| TestRunEntry {
            summary: test_run_summary_from_observations(
                source,
                run,
                projection.observations(&run.disk.run_id).iter().copied(),
            ),
            disk: run.disk.clone(),
            discovered_project_id: run.project_id.clone(),
        })
        .collect()
}

impl<T: std::ops::Deref<Target = StateInner>> TestRunProjectionSource for T {
    fn projection_index(&self) -> &TestRunIndex {
        (**self).projection_index()
    }
    fn projection_registrations(&self) -> &BTreeMap<String, TestRunRegistration> {
        (**self).projection_registrations()
    }
    fn projection_observations(&self) -> &BTreeMap<String, TestRunCommandObservation> {
        (**self).projection_observations()
    }
    fn projection_compacted(&self) -> &BTreeMap<String, TestRunCompacted> {
        (**self).projection_compacted()
    }
    fn projection_project_exists(&self, id: &str) -> bool {
        (**self).projection_project_exists(id)
    }
    fn projection_sessions(&self) -> impl Iterator<Item = TestRunProjectionSessionRef<'_>> {
        (**self).projection_sessions()
    }
    fn projection_carried(&self) -> impl Iterator<Item = TestRunProjectionCarriedRef<'_>> {
        (**self).projection_carried()
    }
    fn projection_session_by_id(&self, id: &str) -> Option<TestRunProjectionSessionRef<'_>> {
        (**self).projection_session_by_id(id)
    }
}

struct TestRunProjectionScope {
    run_ids: HashSet<String>,
    references: HashSet<String>,
    reference_names: HashSet<String>,
    paths: HashSet<String>,
}

impl TestRunProjectionScope {
    fn new(scanned: &[TestRunScanned]) -> Self {
        let references: HashSet<_> = scanned
            .iter()
            .flat_map(|run| run.disk.owner.iter().chain(run.disk.notify_to.iter()))
            .map(|reference| reference.trim().to_owned())
            .collect();
        Self {
            run_ids: scanned.iter().map(|run| run.disk.run_id.clone()).collect(),
            reference_names: references
                .iter()
                .map(|reference| reference.to_ascii_lowercase())
                .collect(),
            references,
            paths: scanned
                .iter()
                .map(|run| test_run_path_key(&test_run_display_path(&run.disk.run_dir)))
                .collect(),
        }
    }
    fn observation(&self, row: &TestRunCommandObservation) -> bool {
        row.artifact
            .as_ref()
            .is_some_and(|artifact| self.run_ids.contains(&artifact.run_id))
    }
    fn session(&self, row: TestRunProjectionSessionRef<'_>) -> bool {
        self.references.contains(row.id)
            || self
                .reference_names
                .contains(&row.name.to_ascii_lowercase())
    }
    fn carried(&self, row: TestRunProjectionCarriedRef<'_>) -> bool {
        row.run_directory.is_some_and(|path| {
            self.paths
                .contains(&test_run_path_key(&test_run_display_path(path)))
        })
    }
}

fn test_run_selected_map_matches<T: PartialEq>(
    saved: &BTreeMap<String, T>,
    live: &BTreeMap<String, T>,
    select: impl Fn(&T) -> bool,
) -> bool {
    let mut count = 0;
    for (key, row) in live.iter().filter(|(_, row)| select(row)) {
        count += 1;
        if saved.get(key) != Some(row) {
            return false;
        }
    }
    count == saved.len()
}

fn test_run_selected_set_matches<T: Eq + std::hash::Hash>(
    saved: &HashSet<T>,
    live: &HashSet<T>,
    select: impl Fn(&T) -> bool,
) -> bool {
    let mut count = 0;
    for value in live.iter().filter(|value| select(value)) {
        count += 1;
        if !saved.contains(value) {
            return false;
        }
    }
    count == saved.len()
}

struct TestRunProjectionCut {
    scope: TestRunProjectionScope,
    observation_ids: HashSet<String>,
    observation_presence: BTreeMap<String, bool>,
    project_ids: HashSet<String>,
    index: TestRunIndex,
    registrations: BTreeMap<String, TestRunRegistration>,
    observations: BTreeMap<String, TestRunCommandObservation>,
    compacted: BTreeMap<String, TestRunCompacted>,
    projects: Vec<String>,
    sessions: Vec<TestRunProjectionSession>,
    carried: Vec<TestRunProjectionCarried>,
}

impl TestRunProjectionCut {
    fn capture(inner: &StateInner, mut scope: TestRunProjectionScope) -> Self {
        // Candidate rows are selected by artifact run id before the shared
        // exact association predicates run. Validation repeats that selection
        // over live rows, so a previously absent/newly relevant association
        // cannot be hidden by retaining only the old candidate keys.
        let observations: BTreeMap<_, _> = inner
            .test_run_observations
            .iter()
            .filter(|(_, row)| scope.observation(row))
            .map(|(key, row)| (key.clone(), row.clone()))
            .collect();
        let compacted: BTreeMap<_, _> = scope
            .run_ids
            .iter()
            .filter_map(|id| {
                inner
                    .test_run_compacted
                    .get(id)
                    .map(|row| (id.clone(), row.clone()))
            })
            .collect();
        scope.paths.extend(
            compacted
                .values()
                .map(|row| test_run_path_key(&row.summary.run_dir)),
        );
        let observation_presence: BTreeMap<_, _> = compacted
            .values()
            .flat_map(|row| &row.observations)
            .map(|row| {
                (
                    row.execution_id.clone(),
                    inner.test_run_observations.contains_key(&row.execution_id),
                )
            })
            .collect();
        // Installation only needs presence of retained execution ids. Do not
        // clone unrelated live rows just to answer that membership question.
        let observation_ids: HashSet<_> = observations
            .values()
            .map(|row| row.execution_id.clone())
            .chain(observation_presence.keys().cloned())
            .collect();
        let registrations: BTreeMap<_, _> = scope
            .run_ids
            .iter()
            .filter_map(|id| {
                inner
                    .test_run_registrations
                    .get(id)
                    .map(|row| (id.clone(), row.clone()))
            })
            .collect();
        let project_ids: HashSet<_> = registrations
            .values()
            .filter_map(|row| row.project_id.clone())
            .collect();
        let run_markers = |markers: &HashSet<String>| {
            scope
                .run_ids
                .iter()
                .filter(|id| markers.contains(*id))
                .cloned()
                .collect()
        };
        let observation_markers = |markers: &HashSet<String>| {
            observation_ids
                .iter()
                .filter(|id| markers.contains(*id))
                .cloned()
                .collect()
        };
        // Only fields read by summary/proof projection, never the disk parse
        // cache, cards, scheduling state, mutexes or persistence waiters.
        let index = TestRunIndex {
            entries: inner
                .test_runs
                .entries
                .iter()
                .filter(|row| scope.run_ids.contains(&row.summary.run_id))
                .cloned()
                .collect(),
            registered: run_markers(&inner.test_runs.registered),
            registration_failures: run_markers(&inner.test_runs.registration_failures),
            observations_durable: observation_markers(&inner.test_runs.observations_durable),
            observations_failed: observation_markers(&inner.test_runs.observations_failed),
            lifecycle_revalidated: inner
                .test_runs
                .lifecycle_revalidated
                .iter()
                .filter(|(id, _)| observation_ids.contains(id))
                .cloned()
                .collect(),
            compactions_durable: run_markers(&inner.test_runs.compactions_durable),
            compactions_revalidating: run_markers(&inner.test_runs.compactions_revalidating),
            scanned_once: inner.test_runs.scanned_once,
            ..TestRunIndex::default()
        };
        Self {
            index,
            registrations,
            observations,
            compacted,
            projects: inner
                .projects
                .iter()
                .filter(|row| project_ids.contains(&row.id))
                .map(|row| row.id.clone())
                .collect(),
            sessions: inner
                .projection_sessions()
                .filter(|row| scope.session(*row))
                .map(|row| TestRunProjectionSession {
                    id: row.id.to_owned(),
                    name: row.name.to_owned(),
                    hidden: row.hidden,
                    child: row.child,
                })
                .collect(),
            carried: inner
                .projection_carried()
                .filter(|row| scope.carried(*row))
                .map(|row| TestRunProjectionCarried {
                    run_directory: row.run_directory.map(FsPath::to_path_buf),
                    terminal_digest: row.terminal_digest.map(str::to_owned),
                    terminal_conflict: row.terminal_conflict,
                    ambiguous: row.ambiguous,
                })
                .collect(),
            scope,
            observation_ids,
            observation_presence,
            project_ids,
        }
    }

    fn matches(&self, inner: &StateInner) -> bool {
        let live = &inner.test_runs;
        // Revision alone cannot fence unstamped durability or metadata writes.
        self.scope.run_ids.iter().all(|id| {
            self.index.registered.contains(id) == live.registered.contains(id)
                && self.index.registration_failures.contains(id)
                    == live.registration_failures.contains(id)
                && self.index.compactions_durable.contains(id)
                    == live.compactions_durable.contains(id)
                && self.index.compactions_revalidating.contains(id)
                    == live.compactions_revalidating.contains(id)
                && self.registrations.get(id) == inner.test_run_registrations.get(id)
                && self.compacted.get(id) == inner.test_run_compacted.get(id)
        }) && self.observation_ids.iter().all(|id| {
            self.index.observations_durable.contains(id) == live.observations_durable.contains(id)
                && self.index.observations_failed.contains(id)
                    == live.observations_failed.contains(id)
        }) && test_run_selected_set_matches(
            &self.index.lifecycle_revalidated,
            &live.lifecycle_revalidated,
            |(id, _)| self.observation_ids.contains(id),
        ) && self.index.scanned_once == live.scanned_once
            && self.index.entries.len()
                == live
                    .entries
                    .iter()
                    .filter(|row| self.scope.run_ids.contains(&row.summary.run_id))
                    .count()
            && self
                .index
                .entries
                .iter()
                .zip(
                    live.entries
                        .iter()
                        .filter(|row| self.scope.run_ids.contains(&row.summary.run_id)),
                )
                .all(|(old, current)| {
                    old.summary == current.summary
                        && old.discovered_project_id == current.discovered_project_id
                        && Arc::ptr_eq(&old.disk, &current.disk)
                })
            && test_run_selected_map_matches(
                &self.observations,
                &inner.test_run_observations,
                |row| self.scope.observation(row),
            )
            && self
                .observation_presence
                .iter()
                .all(|(id, present)| *present == inner.test_run_observations.contains_key(id))
            && self.projects.iter().map(String::as_str).eq(inner
                .projects
                .iter()
                .filter(|row| self.project_ids.contains(&row.id))
                .map(|row| row.id.as_str()))
            && self.projection_sessions().eq(inner
                .projection_sessions()
                .filter(|row| self.scope.session(*row)))
            && self.projection_carried().eq(inner
                .projection_carried()
                .filter(|row| self.scope.carried(*row)))
    }
}

impl TestRunProjectionSource for TestRunProjectionCut {
    fn projection_index(&self) -> &TestRunIndex {
        &self.index
    }
    fn projection_registrations(&self) -> &BTreeMap<String, TestRunRegistration> {
        &self.registrations
    }
    fn projection_observations(&self) -> &BTreeMap<String, TestRunCommandObservation> {
        &self.observations
    }
    fn projection_compacted(&self) -> &BTreeMap<String, TestRunCompacted> {
        &self.compacted
    }
    fn projection_project_exists(&self, id: &str) -> bool {
        self.projects.iter().any(|project| project == id)
    }
    fn projection_has_observation(&self, id: &str) -> bool {
        self.observation_presence.get(id).copied().unwrap_or(false)
    }
    fn projection_sessions(&self) -> impl Iterator<Item = TestRunProjectionSessionRef<'_>> {
        self.sessions.iter().map(|row| TestRunProjectionSessionRef {
            id: &row.id,
            name: &row.name,
            hidden: row.hidden,
            child: row.child,
        })
    }
    fn projection_carried(&self) -> impl Iterator<Item = TestRunProjectionCarriedRef<'_>> {
        self.carried.iter().map(|row| TestRunProjectionCarriedRef {
            run_directory: row.run_directory.as_deref(),
            terminal_digest: row.terminal_digest.as_deref(),
            terminal_conflict: row.terminal_conflict,
            ambiguous: row.ambiguous,
        })
    }
}
