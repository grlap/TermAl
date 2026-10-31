// The committed-input record a launcher run writes into its results.json
// (`input`, for an ordinary committed-input run: `--input commit` in
// scripts/test-launcher.mjs), as the host reads it into a run's detail.
// Owns the record's typed shape, its tolerant read (a run without one, or
// with one that is malformed, contradictory or not a commit input, reads as
// none, never as a panic) and its wire form beside the detail's other fields.
// Does not own the launcher's classification (`commitInputOutcome` in
// scripts/test-launcher.mjs), the rest of the run detail (`test_run_detail`
// in src/test_runs_disk.rs), or any credit, eligibility or disposition
// decision: it reports what the launcher recorded and infers nothing from
// any other field. New module, included beside src/test_runs_disk.rs.

/// The only input kind the launcher records today.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum TestRunCommittedInputKind {
    Commit,
}

/// How the launcher classified the checkout after the run.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum TestRunCommittedInputOutcome {
    /// HEAD unchanged and Git reports nothing.
    Clean,
    /// HEAD unchanged and Git reports only new untracked paths.
    DebrisPendingDisposition,
    /// HEAD moved, or a tracked path changed.
    InputChanged,
}

/// One entry `git status --porcelain=v1` reported, as the launcher keeps it:
/// a rename or copy carries its origin path.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunCommittedInputPath {
    status: String,
    path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin: Option<String>,
}

/// A tracked path the index tells Git to skip, as `git ls-files -v` tags it:
/// a lowercase letter for assume-unchanged, `S` for skip-worktree. Git does
/// not read its worktree copy, so a checkout with one is not clean.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunCommittedInputSkippedPath {
    tag: String,
    path: String,
}

/// The checkout as the launcher found it after the run.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunCommittedInputAfter {
    commit: String,
    clean: bool,
    #[serde(default)]
    paths: Vec<TestRunCommittedInputPath>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    skipped_paths: Vec<TestRunCommittedInputSkippedPath>,
    /// The Git filters (clean, smudge or process keys) configured when the
    /// run ended. Git's status was not read under them, so any is a change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    filters: Vec<String>,
    /// The submodule gitlinks the index held when the run ended. Git's
    /// status was not read with them, so any is a change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    gitlinks: Vec<String>,
}

/// A committed-input run's `input` record. `commit` is absent only for a run
/// refused because its checkout was not clean before the run, whose
/// `dirty_paths`, `skipped_paths`, `filters` and `gitlinks` say why; `after`,
/// `outcome` and `eligible` are absent until
/// the launcher records them. Fields the launcher adds later are ignored.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunCommittedInput {
    kind: TestRunCommittedInputKind,
    #[serde(default)]
    commit: Option<String>,
    clean_before: bool,
    #[serde(default)]
    dirty_paths: Vec<TestRunCommittedInputPath>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    skipped_paths: Vec<TestRunCommittedInputSkippedPath>,
    /// The Git filters (clean, smudge or process keys) configured before the
    /// run, for a run refused because of them; the committed-input status
    /// does not run under a filter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    filters: Vec<String>,
    /// The submodule gitlinks the index held before the run, for a run
    /// refused because of them; the committed-input status does not inspect
    /// a submodule, whose own config could name a filter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    gitlinks: Vec<String>,
    #[serde(default)]
    after: Option<TestRunCommittedInputAfter>,
    #[serde(default)]
    outcome: Option<TestRunCommittedInputOutcome>,
    #[serde(default)]
    eligible: Option<bool>,
}

/// A full Git object id: SHA-1 (40) or SHA-256 (64) lowercase hex.
fn test_run_commit_id_valid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl TestRunCommittedInput {
    /// The record in `results`, or none: absent for a staged-lane run and
    /// for runs older than the record; none, too, for one that does not
    /// parse or contradicts itself, since a half-read record could only
    /// mislead.
    fn read(results: &Value) -> Option<Self> {
        let input: Self = serde_json::from_value(results.get("input")?.clone()).ok()?;
        input.coherent().then_some(input)
    }

    /// Whether the record's parts agree with each other as the launcher
    /// writes them; nothing is filled in from elsewhere.
    fn coherent(&self) -> bool {
        if self.commit.as_deref().is_some_and(|commit| !test_run_commit_id_valid(commit))
            || self
                .after
                .as_ref()
                .is_some_and(|after| !test_run_commit_id_valid(&after.commit))
        {
            return false;
        }
        if !self.clean_before {
            // Refused before any stage: nothing ran, so nothing after.
            return self.after.is_none()
                && self.outcome.is_none()
                && self.eligible != Some(true);
        }
        let Some(commit) = self.commit.as_deref() else {
            return false;
        };
        if !self.dirty_paths.is_empty()
            || !self.skipped_paths.is_empty()
            || !self.filters.is_empty()
            || !self.gitlinks.is_empty()
        {
            return false;
        }
        let after_ok = match (self.outcome, self.after.as_ref()) {
            (None, after) => after.is_none(),
            (Some(_), None) => false,
            (Some(TestRunCommittedInputOutcome::Clean), Some(after)) => {
                after.commit == commit
                    && after.clean
                    && after.paths.is_empty()
                    && after.skipped_paths.is_empty()
                    && after.filters.is_empty()
                    && after.gitlinks.is_empty()
            }
            (Some(TestRunCommittedInputOutcome::DebrisPendingDisposition), Some(after)) => {
                after.commit == commit
                    && !after.clean
                    && !after.paths.is_empty()
                    && after.skipped_paths.is_empty()
                    && after.filters.is_empty()
                    && after.gitlinks.is_empty()
                    && after.paths.iter().all(|path| path.status == "??")
            }
            (Some(TestRunCommittedInputOutcome::InputChanged), Some(after)) => {
                after.commit != commit
                    || !after.skipped_paths.is_empty()
                    || !after.filters.is_empty()
                    || !after.gitlinks.is_empty()
                    || after.paths.iter().any(|path| path.status != "??")
            }
        };
        after_ok
            && (self.eligible != Some(true)
                || self.outcome == Some(TestRunCommittedInputOutcome::Clean))
    }
}
