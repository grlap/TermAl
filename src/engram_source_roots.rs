// The work's source root: the worktree an agent names once for a claimed Engram
// item, which TermAl then measures instead of the session's workdir for every
// source basis of a turn on that claim, for the credit of its tests, and as the
// acceptance evaluator's cwd and declared fingerprint. Owns the persisted
// entries (`EngramWorkSourceRoot`), their validation (a registered worktree of
// the caller's repository inside its project folder), the basis taken on
// exactly the stored path and never on an ancestor's, the lookup by claim, the
// rules that end an entry, and the naming request behind the
// `termal_name_source_root` tool. Does not own when a basis is taken
// (`engram_turn_observations.rs`, `engram_turn_checks.rs`,
// `acceptance_evaluation_api.rs`), the held-claims read
// (`engram_held_claims.rs`) or the MCP tool definition (`delegation_mcp.rs`).
// New file.

/// At most this many named roots are kept host-wide. A full list refuses a
/// new name rather than evict one: an evicted entry would silently send its
/// work back to the session's workdir.
const ENGRAM_WORK_SOURCE_ROOT_LIMIT: usize = 64;

include!("engram_named_root_selection.rs");

/// One work's source root, persisted so a restart keeps it. It applies to the
/// work's claim `claim_id` only, so a new claim on the same work does not
/// inherit an old tree. A same-root name under the live claim refreshes
/// `claim_fence` for new registrations while keeping the name's identity.
/// Confirmed events and existing registrations retain their recorded fence.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramWorkSourceRoot {
    store: EngramAuthorityStoreKey,
    work_id: String,
    short_ref: String,
    claim_id: String,
    claim_fence: i64,
    /// The canonical path as `ReviewFreezeGit` returns it (on Windows with
    /// its verbatim prefix), which is also the basis's `workspace_id`.
    root: String,
    /// `engram_exact_path_key` of the repository's common Git directory,
    /// compared exactly: on a case-sensitive volume two repositories may
    /// differ only in case.
    common_dir_key: String,
    named_by_session: String,
    named_at: String,
    /// Unique host-wide to each new name (`StateInner::
    /// engram_source_root_generation`): naming the same root again under the
    /// same claim keeps it, while another root, another claim or a name after
    /// a clear gets a new one. A turn and an evaluation record the one they
    /// were admitted or requested under.
    generation: u64,
}

impl EngramWorkSourceRoot {
    /// The acknowledged name survives a live-fence refresh. Only the fence
    /// may differ from the immutable naming event; every name field stays pinned.
    fn same_name(&self, other: &Self) -> bool {
        self.store == other.store
            && self.work_id == other.work_id
            && self.short_ref == other.short_ref
            && self.claim_id == other.claim_id
            && self.root == other.root
            && self.common_dir_key == other.common_dir_key
            && self.named_by_session == other.named_by_session
            && self.named_at == other.named_at
            && self.generation == other.generation
    }
}

/// Two source captures, one shared control allowance and a commit reserve.
/// Validation also consumes this wall-clock budget. A runtime may cut the
/// caller's wait sooner; the next identical call reveals a retained result
/// or settles the durable pending intent.
const ENGRAM_SOURCE_ROOT_NAMING_BUDGET: Duration = Duration::from_secs(
    2 * ENGRAM_SOURCE_METADATA_BUDGET.as_secs()
        + ENGRAM_SOURCE_ROOT_COMMIT_RESERVE.as_secs()
        + ENGRAM_MAX_CALL_TIMEOUT_MS / 1_000,
);

/// The part of the naming budget the captures leave for the commit under the
/// lock, so a slow capture reports the new root unmeasured (or seals no
/// revision) rather than spending the budget and refusing a valid name.
const ENGRAM_SOURCE_ROOT_COMMIT_RESERVE: Duration = Duration::from_secs(2);

/// How long one capture of a naming call may take, with `left` of its budget
/// remaining: the source budget, stopping `ENGRAM_SOURCE_ROOT_COMMIT_RESERVE`
/// short of the end.
fn engram_source_root_capture_budget(left: Duration, control_left: Duration) -> Duration {
    let available = left.saturating_sub(ENGRAM_SOURCE_ROOT_COMMIT_RESERVE + control_left);
    if available < ENGRAM_SOURCE_METADATA_BUDGET { Duration::ZERO }
    else { ENGRAM_SOURCE_COMMAND_BUDGET }
}

/// Charge only time spent in control I/O, so filesystem captures between
/// status and bind cannot refresh or exhaust the shared dispatch allowance.
fn engram_source_root_control_within<T>(
    clock: &EngramBudgetClock,
    remaining: &mut Duration,
    wall_left: Duration,
    call: impl FnOnce(Duration) -> Result<T, ApiError>,
) -> Result<T, ApiError> {
    let budget = (*remaining).min(wall_left);
    if budget.is_zero() {
        return Err(ApiError::bad_gateway(
            "named-root control budget exhausted; retry the same request to settle any pending intent",
        ));
    }
    let started = clock.now();
    let result = call(budget);
    let elapsed = clock.elapsed_since(started);
    *remaining = remaining.saturating_sub(elapsed);
    // The shared allowance decides admission, not the outcome of an issued
    // call. Its own operation deadline may outlast the remaining share.
    // Preserve both completed replies and typed failures; exhaustion refuses
    // the next unissued phase instead of discarding an applied write.
    result
}

// Maintenance is optional here: reserve a full mandatory control window
// before admitting another full operation. Otherwise maintenance is skipped
// so it cannot consume this naming request's mandatory allowance.
fn engram_source_root_optional_control_within(
    clock: &EngramBudgetClock,
    remaining: &mut Duration,
    wall_left: Duration,
    call: impl FnOnce(Duration) -> Result<(), ApiError>,
) -> Result<(), ApiError> {
    let share = (*remaining).min(wall_left).saturating_sub(ENGRAM_MIN_CALL_TIMEOUT);
    if share < ENGRAM_MIN_CALL_TIMEOUT {
        return Ok(());
    }
    engram_source_root_control_within(clock, remaining, share, call)
}

#[cfg(test)]
thread_local! {
    /// Run once on this test thread right after a naming call's held-claims
    /// read (`AppState::name_engram_source_root`), so a test can land a name
    /// between that read and the call's commit.
    static TEST_ENGRAM_AFTER_SOURCE_ROOT_HELD_READ: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    /// Run once on this test thread right after a naming bind's answer
    /// arrives and before it is adopted, so a test can replace the session's
    /// binding meanwhile.
    static TEST_ENGRAM_AFTER_NAMING_BIND_REPLY: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    /// While set on this test thread, saving an adopted naming bind fails
    /// without writing.
    static TEST_ENGRAM_NAMING_BIND_SAVE_FAILS: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// At most this many host lines about source roots wait for the agent's
/// next prompt; older ones give way.
const ENGRAM_SOURCE_ROOT_PENDING_LINES: usize = 4;

struct EngramOpeningPrompt {
    base: String,
}

/// Current status, separate from historical naming/check messages. Runtime
/// only; equal prose for a successor is a different delivery instance.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EngramSourceRootNotice {
    id: String,
    kind: EngramSourceRootNoticeKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum EngramSourceRootNoticeKind {
    AuthorityRecovery {
        store: EngramAuthorityStoreKey,
        work_id: String,
        line: String,
    },
    SelectionLoss {
        selection: EngramWorkSourceRoot,
    },
    Reclaimed {
        selection: EngramWorkSourceRoot,
        line: String,
    },
}

impl EngramSourceRootNotice {
    fn scope(&self) -> (&EngramAuthorityStoreKey, &str, Option<&str>) {
        match &self.kind {
            EngramSourceRootNoticeKind::AuthorityRecovery { store, work_id, .. } => {
                (store, work_id, None)
            }
            EngramSourceRootNoticeKind::SelectionLoss { selection }
            | EngramSourceRootNoticeKind::Reclaimed { selection, .. } => (
                &selection.store,
                &selection.work_id,
                Some(&selection.claim_id),
            ),
        }
    }

    fn line(&self) -> &str {
        match &self.kind {
            EngramSourceRootNoticeKind::AuthorityRecovery { line, .. }
            | EngramSourceRootNoticeKind::Reclaimed { line, .. } => line,
            EngramSourceRootNoticeKind::SelectionLoss { .. } => {
                "[TermAl] Engram no longer confirms this claim's local source-root selection. Name its worktree again before editing or testing there."
            }
        }
    }

    fn retired(&self, inner: &StateInner) -> bool {
        if matches!(self.kind, EngramSourceRootNoticeKind::Reclaimed { .. }) {
            return !inner
                .engram_work_naming_history
                .iter()
                .flat_map(|history| &history.retirements)
                .any(|notice| notice.id == self.id && notice.published && !notice.delivered);
        }
        let (store, work, _) = self.scope();
        if engram_authority_work_unresolved(inner, store, work) {
            return false;
        }
        let EngramSourceRootNoticeKind::SelectionLoss { selection: lost } = &self.kind else {
            return true;
        };
        let Some(history) = inner
            .engram_work_naming_history
            .iter()
            .find(|history| &history.store == store && history.work_id == work)
        else {
            return false;
        };
        let proof = history
            .proofs
            .iter()
            .find(|proof| proof.binding.claim_id == lost.claim_id);
        let Some(proof) = proof else {
            return false;
        };
        if matches!(proof.read.run.state.as_str(), "completed" | "cancelled")
            || matches!(
                proof.read.named_root,
                EngramNamedRootState::UnboundByRelease { .. }
            )
        {
            return true;
        }
        // A pending local name is not restoration: require the published
        // work image and its exact canonical binding, not tool success prose.
        engram_work_source_root_for_claim(
            &inner.engram_work_source_roots,
            store,
            work,
            &lost.claim_id,
        )
        .is_some_and(|current| {
            current.generation >= lost.generation
                && matches!(&proof.read.named_root,
                    EngramNamedRootState::Bound { workspace_id, generation, named_at }
                    if workspace_id == &current.root
                        && *generation == current.generation as i64
                        && engram_named_at_matches(named_at, &current.named_at))
        })
    }

    fn applies_to(&self, record: &SessionRecord) -> bool {
        if let EngramSourceRootNoticeKind::Reclaimed { selection, .. } = &self.kind {
            return selection.named_by_session == record.session.id;
        }
        let (store, work, claim) = self.scope();
        record
            .engram
            .observed_naming_identity()
            .as_ref()
            .is_some_and(|(active_store, active_work)| active_store == store && active_work == work)
            && claim.is_none_or(|claim| {
                record
                    .engram
                    .observed_opening()
                    .is_some_and(|opening| opening.binding.work_id == work && opening.binding.claim_id == claim)
            })
    }
}

fn refresh_engram_source_root_notices_locked(
    inner: &mut StateInner,
    index: usize,
) -> Vec<EngramSourceRootNotice> {
    let session_id = &inner.sessions[index].session.id;
    let restored = inner
        .engram_work_naming_history
        .iter()
        .flat_map(|history| &history.retirements)
        .filter(|notice| {
            notice.published
                && !notice.delivered
                && notice.selection.named_by_session == *session_id
        })
        .map(|notice| EngramSourceRootNotice {
            id: notice.id.clone(),
            kind: EngramSourceRootNoticeKind::Reclaimed {
                selection: notice.selection.clone(),
                line: notice.line.clone(),
            },
        })
        .collect::<Vec<_>>();
    for notice in restored {
        if !inner.sessions[index]
            .engram
            .source_root_notices
            .contains(&notice)
        {
            inner.sessions[index]
                .engram
                .source_root_notices
                .push(notice);
        }
    }
    let pending = std::mem::take(&mut inner.sessions[index].engram.source_root_notices);
    // Scope mismatch only suppresses delivery. No producer is guaranteed to
    // recreate the notice when this session returns to the still-live claim.
    let retained: Vec<_> = pending
        .into_iter()
        .filter(|notice| !notice.retired(inner))
        .collect();
    let delivered = retained
        .iter()
        .filter(|notice| notice.applies_to(&inner.sessions[index]))
        .cloned()
        .collect();
    inner.sessions[index].engram.source_root_notices = retained;
    delivered
}

fn acknowledge_engram_source_root_notices_locked(
    record: &mut SessionRecord,
    delivered: &[EngramSourceRootNotice],
) {
    // The current handoff holds StateInner through queue acceptance. Keeping
    // exact-instance equality also fences a delayed acknowledgement helper.
    record
        .engram
        .source_root_notices
        .retain(|notice| !delivered.contains(notice));
}

impl EngramSessionState {
    fn queue_source_root_notice(&mut self, kind: EngramSourceRootNoticeKind) {
        let notice = EngramSourceRootNotice {
            id: Uuid::new_v4().to_string(),
            kind,
        };
        // Replace only a newer current status for this exact scope and kind.
        // Another live claim's undelivered instruction remains pending.
        self.source_root_notices
            .retain(|prior| prior.scope() != notice.scope());
        self.source_root_notices.push(notice);
    }
}

/// Compose current ordinary root notices into this exact turn's prompt.
/// The owned composition slot keeps user text intact across refreshes.
fn refresh_engram_source_root_prompt_locked(
    record: &mut SessionRecord,
    dispatch: &mut TurnDispatch,
    notices: &[EngramSourceRootNotice],
    opening_unconfirmed: Option<EngramUnconfirmedRootReason>,
) {
    if record.active_turn_generation != dispatch.active_turn_generation() { return; }
    let (prompt, slot) = match dispatch {
        TurnDispatch::PersistentClaude { command, opening_prompt, .. } => (&mut command.text, opening_prompt),
        TurnDispatch::PersistentCodex { command, opening_prompt, .. } => (&mut command.prompt, opening_prompt),
        TurnDispatch::PersistentAcp { command, opening_prompt, .. } => (&mut command.prompt, opening_prompt),
    };
    let native_notice = declared_run_opening_notice(record);
    if notices.is_empty() && opening_unconfirmed.is_none() && native_notice.is_none() && slot.is_none() { return; }
    let base = &slot.get_or_insert_with(|| EngramOpeningPrompt { base: prompt.clone() }).base;
    let mut lines: Vec<_> = notices.iter().map(|notice| notice.line().to_owned()).collect();
    lines.extend(native_notice);
    if let Some(reason) = opening_unconfirmed {
        lines.push(format!(
            "[TermAl] This turn's source root is unconfirmed. Your work is not blocked: Git, build, test and tool actions run normally. Only this turn's source, check and evaluation evidence is withheld from credit. Any shown path is the last acknowledged local selection. To earn credit in a later turn, name the worktree or refresh its authority; neither takes effect in this turn. Reason: {}.",
            reason.sentence()
        ));
    }
    *prompt = if lines.is_empty() { base.clone() } else { format!("{}\n\n{base}", lines.join("\n")) };
}

/// Recognized command feedback shares one slot, separate from root guidance.
const ENGRAM_UNCREDITED_TEST_LINE_PREFIX: &str = "[TermAl] A recognised test";
const ENGRAM_UNCREDITED_BUILD_LINE_PREFIX: &str = "[TermAl] A recognised build";

fn engram_is_uncredited_check_line(line: &str) -> bool {
    line.starts_with(ENGRAM_UNCREDITED_TEST_LINE_PREFIX)
        || line.starts_with(ENGRAM_UNCREDITED_BUILD_LINE_PREFIX)
}

impl EngramSessionState {
    /// Starts and descriptions share one publication marker for the command.
    /// Placement-only guidance leaves that marker free for later admission.
    fn set_pending_uncredited_command_line(&mut self, key: &str, line: Option<String>) {
        if let Some(line) = line {
            self.withheld_command_keys.insert(key.to_owned());
            if self.uncredited_command_notice_keys.insert(key.to_owned()) {
                self.set_pending_source_root_line(line);
            }
        }
    }

    /// Adds a host line for the agent's next prompt about where its turns on
    /// claimed work are measured. Lines not yet delivered are kept, so a
    /// later one (a withheld test) does not hide an earlier one (a bind); a
    /// line already pending is not repeated but moves to the newest place,
    /// so the last line always tells the latest state (name, clear, name
    /// again ends on the name); only the newest
    /// `ENGRAM_SOURCE_ROOT_PENDING_LINES` are kept. A line given again after
    /// a prompt carrying it was built is a new line: that prompt's
    /// acceptance does not take it, or an older line would end the next
    /// prompt's lines. The lines are kept one per text line, so a line break
    /// inside one (a path may hold one on Linux and macOS) becomes a space.
    /// Feedback about an uncredited test or build replaces earlier check
    /// feedback, so a run of refused checks never evicts a bind or name line.
    fn set_pending_source_root_line(&mut self, line: String) {
        let line = line.replace(['\r', '\n'], " ");
        #[cfg(test)]
        if let Some(lines) = self.test_source_root_lines.as_mut() {
            lines.push(line.clone());
        }
        let mut lines = self
            .pending_source_root_line
            .as_deref()
            .map(|pending| pending.lines().map(str::to_owned).collect::<Vec<_>>())
            .unwrap_or_default();
        let uncredited = engram_is_uncredited_check_line(&line);
        if uncredited {
            self.uncredited_feedback_revision = self.uncredited_feedback_revision.wrapping_add(1);
        }
        // Lines about a check's credit merge into one, so several of them
        // never push a bind or name line out. One a prompt in flight carries
        // is left as it is, so its delivery takes it out when the runtime
        // accepts that prompt, and the new line waits on its own.
        let line = if line.starts_with(ENGRAM_CHECK_CREDIT_LINE_PREFIX) {
            let in_flight = self
                .source_root_line_delivery
                .as_ref()
                .map(|(delivered, _)| delivered.lines().map(str::to_owned).collect::<Vec<_>>())
                .unwrap_or_default();
            match lines.iter().position(|pending| {
                pending.starts_with(ENGRAM_CHECK_CREDIT_LINE_PREFIX) && !in_flight.contains(pending)
            }) {
                Some(position) => {
                    let earlier = lines.remove(position);
                    engram_merge_credit_lines(&earlier, &line)
                }
                None => line,
            }
        } else {
            line
        };
        lines.retain(|pending| {
            *pending != line && !(uncredited && engram_is_uncredited_check_line(pending))
        });
        if let Some((delivered, _)) = self.source_root_line_delivery.as_mut() {
            *delivered = delivered
                .lines()
                .filter(|delivered| *delivered != line)
                .collect::<Vec<_>>()
                .join("\n");
        }
        lines.push(line);
        let excess = lines.len().saturating_sub(ENGRAM_SOURCE_ROOT_PENDING_LINES);
        lines.drain(..excess);
        self.pending_source_root_line = Some(lines.join("\n"));
        // A repair line pushed out of the pending lines takes its origin.
        self.prune_retained_history_repair_origins();
    }

    /// `set_pending_source_root_line` for a retained-history repair line about
    /// `work` in `store`, recording that origin so that only an acknowledged
    /// forward name of exactly that store and work retires it
    /// (`retire_retained_history_repair_lines`). Origins whose line is no
    /// longer pending are dropped first, so a delivered line given again, in
    /// the same words, carries only its new origin; the generic setter then
    /// drops those of a line the newest one pushed out.
    fn set_pending_retained_history_repair_line(
        &mut self,
        line: String,
        store: &EngramAuthorityStoreKey,
        work: &str,
    ) {
        let line = line.replace(['\r', '\n'], " ");
        self.prune_retained_history_repair_origins();
        self.set_pending_source_root_line(line.clone());
        if !self.retained_history_repair_origins.iter().any(|(origin, origin_store, origin_work)| {
            *origin == line && origin_store == store && origin_work == work
        }) {
            self.retained_history_repair_origins.push((line, store.clone(), work.to_owned()));
        }
    }

    /// Drops the repair origins whose line is no longer pending: delivered
    /// and acknowledged, pushed out by newer lines, or retired.
    fn prune_retained_history_repair_origins(&mut self) {
        let pending = self.pending_source_root_line.as_deref().unwrap_or_default();
        self.retained_history_repair_origins
            .retain(|(origin, _, _)| pending.lines().any(|pending| pending == origin));
    }

    /// Retires the pending repair lines given for `work` in `store`, once an
    /// acknowledged forward name made that history current. A line also given
    /// for another store or work stays pending, as does every other line, in
    /// order. A prompt already built keeps its text, and its delivery takes
    /// out only what is still pending.
    fn retire_retained_history_repair_lines(&mut self, store: &EngramAuthorityStoreKey, work: &str) {
        // An origin whose line already left can neither be retired nor hold
        // a line given again for another origin.
        self.prune_retained_history_repair_origins();
        let (retired, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.retained_history_repair_origins)
            .into_iter()
            .partition(|(_, origin_store, origin_work)| origin_store == store && origin_work == work);
        let retired = retired
            .into_iter()
            .map(|(line, _, _)| line)
            .filter(|line| !kept.iter().any(|(still, _, _)| still == line))
            .collect::<HashSet<_>>();
        self.retained_history_repair_origins = kept;
        if retired.is_empty() {
            return;
        }
        let Some(pending) = self.pending_source_root_line.as_deref() else {
            return;
        };
        let remaining = pending
            .lines()
            .filter(|line| !retired.contains(*line))
            .collect::<Vec<_>>();
        self.pending_source_root_line = (!remaining.is_empty()).then(|| remaining.join("\n"));
    }
}

/// Where a source basis is taken: the session's workdir, as before phase 2,
/// or a work's named root, on exactly that path.
#[derive(Clone, Debug, PartialEq, Eq)]
enum EngramBasisPlace {
    Workdir(String),
    Named {
        root: String,
        common_dir_key: String,
    },
}

/// The root a turn was admitted with, kept on the session record while that
/// turn runs. Runtime only: a turn does not survive a restart.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramTurnSourceRoot {
    root: String,
    common_dir_key: String,
    short_ref: String,
    claim_id: String,
    /// The name's generation (`EngramWorkSourceRoot::generation`), which a
    /// check carried past this turn keeps to settle only under the same name.
    generation: u64,
    /// Historical string seals stay readable as stored. New naming never
    /// constructs one or derives a full identity from one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sealed_revision: Option<String>,
    /// Complete owner identity at rename/clear, pinned to this original turn.
    /// It knows of no edit made after the seal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sealed_source_id: Option<EngramSourceId>,
}

impl EngramTurnSourceRoot {
    fn from_entry(entry: &EngramWorkSourceRoot) -> Self {
        Self {
            root: entry.root.clone(),
            common_dir_key: entry.common_dir_key.clone(),
            short_ref: entry.short_ref.clone(),
            claim_id: entry.claim_id.clone(),
            generation: entry.generation,
            sealed_revision: None,
            sealed_source_id: None,
        }
    }

    fn place(&self) -> EngramBasisPlace {
        EngramBasisPlace::Named {
            root: self.root.clone(),
            common_dir_key: self.common_dir_key.clone(),
        }
    }
}

/// The named source root an acceptance evaluation was requested on, recorded
/// on its target: the evaluator's cwd and fingerprint were taken there, and
/// its first submission is refused if the work's root has changed since
/// (another claim, another generation, another path).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceEvaluationSourceRoot {
    root: String,
    common_dir_key: String,
    work_id: String,
    claim_id: String,
    generation: u64,
}

impl AcceptanceEvaluationSourceRoot {
    fn from_entry(entry: &EngramWorkSourceRoot) -> Self {
        Self {
            root: entry.root.clone(),
            common_dir_key: entry.common_dir_key.clone(),
            work_id: entry.work_id.clone(),
            claim_id: entry.claim_id.clone(),
            generation: entry.generation,
        }
    }

    fn place(&self) -> EngramBasisPlace {
        EngramBasisPlace::Named {
            root: self.root.clone(),
            common_dir_key: self.common_dir_key.clone(),
        }
    }

    /// Whether the work's entry in `store` still names this root, under the
    /// same claim and generation.
    fn still_named(
        &self,
        entries: &[EngramWorkSourceRoot],
        store: &EngramAuthorityStoreKey,
    ) -> bool {
        engram_work_source_root_for_claim(entries, store, &self.work_id, &self.claim_id)
            .is_some_and(|entry| entry.generation == self.generation && entry.root == self.root)
    }
}

/// The requested work's live claim when an evaluation was requested with
/// no root named, recorded on its target: the claim, not the
/// session's binding at submission, decides whether a root was named since.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceEvaluationSourceClaim {
    work_id: String,
    claim_id: String,
    /// Host bookkeeping: detect a later name for a different current run,
    /// including when that name is cleared before admission or submission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    named_generation_at_request: Option<u64>,
}

/// Why an evaluation's tree is no longer the one its work is measured in,
/// so that its refusal names the cause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AcceptanceEvaluationRootChange {
    /// The recorded root is no longer named under its claim and generation.
    NoLongerNamed,
    /// With no root recorded, a root was named since for the work.
    NamedSince,
    /// The claim's binding was displaced or awaits reconciliation.
    Unsettled,
    /// The work's naming moved, or a naming of it is pending.
    Naming(EngramWorkNamingChange),
    /// The target carries no naming token to fence its tree.
    NoNamingToken,
}

/// The submission's refusal of `change` for `target`, with the in-flight
/// context of the evaluated root's claim when the change is in flight.
fn acceptance_evaluation_submission_root_refusal_locked(
    inner: &StateInner,
    target: &DelegationAcceptanceEvaluation,
    change: AcceptanceEvaluationRootChange,
) -> ApiError {
    let context = (change == AcceptanceEvaluationRootChange::Naming(EngramWorkNamingChange::InFlight))
        .then(|| {
            let store = target.store.as_ref()?;
            let (work_id, claim_id) = match (&target.source_root, &target.source_claim) {
                (Some(root), _) => (&root.work_id, &root.claim_id),
                (None, Some(claim)) => (&claim.work_id, &claim.claim_id),
                (None, None) => return None,
            };
            acceptance_evaluation_in_flight_context_locked(inner, store, work_id, claim_id)
        })
        .flatten();
    acceptance_evaluation_source_root_changed_error(target.source_root.as_ref(), change, context.as_ref())
}

/// Whether the root the evaluation `target` was requested on is no longer
/// the one its work is measured in: a recorded root the work no longer names
/// under its claim and generation, or, with none recorded, a root named since
/// for the claim it was requested under (`source_claim`) on the evaluated
/// work, which moved the work's measurements away from the tree evaluated;
/// a binding displaced or awaiting reconciliation; or a moved or pending
/// naming of the work. History that only learned more of the same binding is
/// no change. The structural causes are named first, as the most specific.
fn acceptance_evaluation_root_change_locked(
    inner: &StateInner,
    target: &DelegationAcceptanceEvaluation,
) -> Option<AcceptanceEvaluationRootChange> {
    let store = target.store.as_ref()?;
    let structural = match (&target.source_root, &target.source_claim) {
        (Some(source_root), _) => !source_root.still_named(&inner.engram_work_source_roots, store),
        (None, Some(claim)) => {
            acceptance_evaluation_other_claim_named_since(inner, store, claim)
                || acceptance_evaluation_claim_named_since(
                    &inner.engram_work_source_roots,
                    store,
                    claim,
                    &target.work_ref,
                )
        }
        (None, None) => false,
    };
    if structural {
        return Some(if target.source_root.is_some() {
            AcceptanceEvaluationRootChange::NoLongerNamed
        } else {
            AcceptanceEvaluationRootChange::NamedSince
        });
    }
    let claim_id = target
        .source_root
        .as_ref()
        .map(|root| root.claim_id.as_str())
        .or_else(|| {
            target
                .source_claim
                .as_ref()
                .map(|claim| claim.claim_id.as_str())
        });
    if claim_id.is_some_and(|claim| {
        engram_root_journal(&inner.engram_named_root_journal, store, claim).is_some_and(|journal| {
            journal.requires_reconciliation()
                || journal.retirement == Some(EngramRootRetirement::Displaced)
        })
    }) {
        return Some(AcceptanceEvaluationRootChange::Unsettled);
    }
    match &target.naming_history {
        None => Some(AcceptanceEvaluationRootChange::NoNamingToken),
        Some(token) => engram_work_naming_change(inner, store, token)
            .map(AcceptanceEvaluationRootChange::Naming),
    }
}

#[cfg(test)]
fn acceptance_evaluation_root_changed_locked(
    inner: &StateInner,
    target: &DelegationAcceptanceEvaluation,
) -> bool {
    acceptance_evaluation_root_change_locked(inner, target).is_some()
}

/// What an in-flight refusal adds once the automatic pass has attempted the
/// same-binding root (engram_named_root_reclamation.rs). Diagnostic only:
/// it changes no classification, status code, retry count, wait or deadline.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AcceptanceEvaluationInFlightContext {
    /// The root's reclamation status, as the named-root status view shows it.
    status: String,
    /// The root's cooldown left when this was captured.
    cooldown: Duration,
}

impl AcceptanceEvaluationInFlightContext {
    /// A suggested delay for a new request: the cooldown left at capture,
    /// rounded up, then the usual hint. Never a promise that it settles.
    fn suggested_retry_after_secs(&self) -> u64 {
        let cooldown = self.cooldown.as_secs() + u64::from(self.cooldown.subsec_nanos() > 0);
        cooldown + ACCEPTANCE_EVALUATION_IN_FLIGHT_RETRY_AFTER.as_secs()
    }
}

/// The context of an in-flight refusal for the exact root `claim_id` names
/// for `work_id` in `store`, captured now: only when the automatic pass
/// has a recorded attempt on that root. Reads held state only.
fn acceptance_evaluation_in_flight_context_locked(
    inner: &StateInner,
    store: &EngramAuthorityStoreKey,
    work_id: &str,
    claim_id: &str,
) -> Option<AcceptanceEvaluationInFlightContext> {
    let root = engram_work_source_root_for_claim(&inner.engram_work_source_roots, store, work_id, claim_id)?;
    let now = inner.engram_budget_clock_snapshot().now();
    engram_root_pass_attempt_text(inner, root, now)?;
    let cooldown = inner
        .engram_root_reclamation
        .cooldowns
        .iter()
        .filter(|(selection, _)| selection == root)
        .map(|(_, until)| until.saturating_duration_since(now))
        .max()
        .unwrap_or_default();
    Some(AcceptanceEvaluationInFlightContext {
        status: engram_named_root_reclamation_text(inner, root, now),
        cooldown,
    })
}

/// The in-flight context of a naming token's own binding.
fn acceptance_evaluation_token_in_flight_context_locked(
    inner: &StateInner,
    store: &EngramAuthorityStoreKey,
    token: &EngramWorkNamingToken,
) -> Option<AcceptanceEvaluationInFlightContext> {
    let bound = token.named_binding.as_ref()?;
    acceptance_evaluation_in_flight_context_locked(inner, store, &token.work_id, &bound.claim_id)
}

/// The refusal of an evaluation request whose naming token no longer fences
/// the work's tree, naming why. `context` is used only for `InFlight`.
fn acceptance_evaluation_naming_refusal(
    lead: &str,
    change: EngramWorkNamingChange,
    context: Option<&AcceptanceEvaluationInFlightContext>,
) -> ApiError {
    let cause = match change {
        EngramWorkNamingChange::Moved => {
            "the work's naming history has since moved in a way not proved to describe the \
             same binding, or now belongs to another authority epoch"
        }
        EngramWorkNamingChange::Pending => {
            "a naming of the work is pending (an intent awaiting Engram's receipt, a run \
             awaiting recovery, or a binding awaiting reconciliation)"
        }
        EngramWorkNamingChange::InFlight => {
            let message = match context {
                None => format!(
                    "{ACCEPTANCE_EVALUATION_AUTHORITY_IN_FLIGHT}: {lead}: the work's authority \
                     is between a maintenance pass's preparation and its publication; request \
                     the evaluation again in {} seconds",
                    ACCEPTANCE_EVALUATION_IN_FLIGHT_RETRY_AFTER.as_secs()
                ),
                Some(context) => format!(
                    "{ACCEPTANCE_EVALUATION_AUTHORITY_IN_FLIGHT}: {lead}: the work's authority \
                     is between a maintenance pass's preparation and its publication; its named \
                     root's reclamation status when this request captured it: {}. As a \
                     suggested delay, not a promise that the transition settles: request the \
                     evaluation again in {} seconds",
                    context.status,
                    context.suggested_retry_after_secs()
                ),
            };
            return ApiError::from_status(StatusCode::SERVICE_UNAVAILABLE, message);
        }
    };
    ApiError::conflict(format!("{lead}: {cause}; request a fresh acceptance evaluation"))
}

/// The start of every refusal that an authority transition in flight
/// causes: retryable, never final (see `EngramWorkNamingChange::InFlight`).
/// The host does not wait the transition out: its submit budget is already
/// the sum of its stages' own deadlines, so a wait would widen it. The
/// caller, which holds the attempt, retries instead.
const ACCEPTANCE_EVALUATION_AUTHORITY_IN_FLIGHT: &str = "authority transition in flight";
/// How long a caller waits before trying again: the scheduler interval and a
/// second. A suggested delay, not a promise: a pass shares its budget among
/// its planned roots, and a root it keeps waits out its cooldown, which the
/// request's refusal then names (`AcceptanceEvaluationInFlightContext`).
const ACCEPTANCE_EVALUATION_IN_FLIGHT_RETRY_AFTER: Duration =
    ENGRAM_ROOT_RECLAMATION_INTERVAL.saturating_add(Duration::from_secs(1));
/// How many times an evaluator submits the same verdicts again after an
/// in-flight refusal before it reports that refusal.
const ACCEPTANCE_EVALUATION_IN_FLIGHT_RETRIES: usize = 3;

/// A fresh name for the work under another current claim invalidates a
/// previously seeded workdir evaluation. Retired historical claims cannot
/// become the new target merely because their selection has disappeared.
fn acceptance_evaluation_other_claim_named_since(
    inner: &StateInner,
    store: &EngramAuthorityStoreKey,
    claim: &AcceptanceEvaluationSourceClaim,
) -> bool {
    let after = claim.named_generation_at_request.unwrap_or(0);
    let replaced = |root: &EngramWorkSourceRoot| {
        &root.store == store
            && root.work_id == claim.work_id
            && root.claim_id != claim.claim_id
            && root.generation > after
    };
    inner.engram_work_source_roots.iter().any(&replaced)
        || inner.engram_named_root_journal.iter().any(|journal| {
            &journal.store == store
                && (journal
                    .pending
                    .as_ref()
                    .is_some_and(|event| replaced(&event.root))
                    || !journal.obsolete
                        && journal
                            .confirmed
                            .as_ref()
                            .is_some_and(|(event, _)| replaced(&event.root)))
        })
}

/// Whether a root is now named for `claim`, the claim an evaluation with no
/// root was requested under, on the evaluated work `work_ref`: the work's
/// measurements have moved away from the workdir it was evaluated in. The
/// recorded claim decides, whatever the requesting session is bound to now.
fn acceptance_evaluation_claim_named_since(
    entries: &[EngramWorkSourceRoot],
    store: &EngramAuthorityStoreKey,
    claim: &AcceptanceEvaluationSourceClaim,
    work_ref: &str,
) -> bool {
    engram_work_source_root_for_claim(entries, store, &claim.work_id, &claim.claim_id)
        .is_some_and(|entry| entry.short_ref == work_ref || entry.work_id == work_ref)
}

/// The refusal of an evaluation's first submission once the root it was
/// requested on (`None`: the session's workdir) is no longer the one its
/// work is measured in, naming the `change` found. The evaluator is told to
/// treat it as final; the host enforces that only for a recorded root, whose
/// generation never returns (with none recorded, a clear of the first name
/// would admit a retry).
fn acceptance_evaluation_source_root_changed_error(
    source_root: Option<&AcceptanceEvaluationSourceRoot>,
    change: AcceptanceEvaluationRootChange,
    context: Option<&AcceptanceEvaluationInFlightContext>,
) -> ApiError {
    let requested_on = match source_root {
        Some(source_root) => format!(
            "the evaluation was requested on {} (generation {})",
            source_root.root, source_root.generation
        ),
        None => "the evaluation was requested on the session's workdir".to_owned(),
    };
    if change == AcceptanceEvaluationRootChange::Naming(EngramWorkNamingChange::InFlight) {
        // Retryable: the pass publishes the same binding or a retirement, and
        // the next submission is judged on whichever it published. The
        // context adds what the last pass attempt did; the wait stays.
        let status = context.map_or_else(String::new, |context| {
            format!(" Its named root's reclamation status now: {}.", context.status)
        });
        return ApiError::from_status(
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "{ACCEPTANCE_EVALUATION_AUTHORITY_IN_FLIGHT}: {requested_on}, and the work's \
                 authority is between a maintenance pass's preparation and its publication.{status} \
                 Nothing was recorded and this attempt is still open: wait {} seconds, then \
                 submit exactly the same verdicts again",
                ACCEPTANCE_EVALUATION_IN_FLIGHT_RETRY_AFTER.as_secs()
            ),
        );
    }
    let lead = match change {
        AcceptanceEvaluationRootChange::Naming(EngramWorkNamingChange::Pending) => {
            "the work's naming is unsettled while it was evaluated"
        }
        _ => "the work's source root changed while it was evaluated",
    };
    let cause = match change {
        AcceptanceEvaluationRootChange::NoLongerNamed => {
            "which the work no longer names under that claim"
        }
        AcceptanceEvaluationRootChange::NamedSince => {
            "and the work has since been given a named source root"
        }
        AcceptanceEvaluationRootChange::Unsettled => {
            "and that claim's named-root binding has since been displaced by another claim's \
             naming or awaits reconciliation with Engram"
        }
        AcceptanceEvaluationRootChange::Naming(EngramWorkNamingChange::Moved) => {
            "and the work's naming history has since moved in a way not proved to describe \
             that same binding, or to another authority epoch"
        }
        AcceptanceEvaluationRootChange::Naming(EngramWorkNamingChange::Pending) => {
            "and a naming of the work is now pending (an intent awaiting Engram's receipt, a \
             run awaiting recovery, or a binding awaiting reconciliation), so the tree it \
             measures is not settled"
        }
        // Answered as retryable above; kept for an exhaustive match.
        AcceptanceEvaluationRootChange::Naming(EngramWorkNamingChange::InFlight) => {
            "and the work's authority is between a maintenance pass's preparation and its \
             publication"
        }
        AcceptanceEvaluationRootChange::NoNamingToken => {
            "but it carries no naming token that could fence that tree"
        }
    };
    ApiError::conflict(format!(
        "{lead}: {requested_on}, {cause}, so these verdicts may not describe its tree. Treat \
         this as final, even if the root changes back: do not submit again. Finish and report \
         this; the parent must request a new evaluation"
    ))
}

/// The source root of the work an acceptance evaluation of `work_ref` (with
/// `work_id` when the tracker's receipt carried it) judges, as the requesting
/// session holds it: the entry of the requested work's live claim. When an
/// independent requester holds no claim, retain the canonical work's entry;
/// Engram's sighting preflight must still confirm its exact path/generation.
/// The control session's bound claim does not select an evaluation's tree.
fn engram_evaluation_source_root(
    entries: &[EngramWorkSourceRoot],
    store: &EngramAuthorityStoreKey,
    claim: Option<&AcceptanceEvaluationSourceClaim>,
    work_ref: &str,
    work_id: Option<&str>,
) -> Option<AcceptanceEvaluationSourceRoot> {
    let selected = match claim {
        Some(claim) => engram_work_source_root_for_claim(entries, store, &claim.work_id, &claim.claim_id),
        None => work_id.and_then(|work| engram_work_source_root_for_work(entries, store, work)),
    };
    selected
        .filter(|entry| {
            work_id.map_or(entry.short_ref == work_ref, |work_id| {
                entry.work_id == work_id
            })
        })
        .map(AcceptanceEvaluationSourceRoot::from_entry)
}

/// The one host line an agent gets, before its next prompt, about where its
/// turns on claimed work are measured: at a newly bound claim, after it names
/// or clears a root, and once a recognised test started outside the turn's root
/// (`engram_source_root_withheld_line`). `work` names the work when no entry
/// does; `one_call` says whether the session is told the one-call form up
/// front (`engram_one_call_offered_up_front`).
fn engram_source_root_line(
    entry: Option<&EngramWorkSourceRoot>,
    work: &str,
    workdir: &str,
    one_call: bool,
) -> String {
    match entry {
        Some(entry) => {
            let root = engram_source_root_display(&entry.root);
            // The one-call form is named only to a session whose runtime
            // reports no directory and runs the bare line (`one_call`,
            // `engram_one_call_offered_up_front`): one that reports a
            // directory is credited where it says it runs, and one that
            // wraps its lines could not run the form. And only for a root
            // the form reads (`engram_one_call_reads`: one holding `$`, `(`
            // or a network prefix would not count).
            let one_call = (one_call && engram_one_call_reads(&root))
                .then(|| {
                    format!(
                        ": start each in one call as `pushd \"DIR\" && <test>`, with DIR the \
                         absolute directory in it where the test \
                         runs{ENGRAM_ONE_CALL_DIR_SPELLING} and nothing piped, redirected or \
                         chained after the test, because a directory changed in an earlier call \
                         may not hold"
                    )
                })
                .unwrap_or_default();
            format!(
                "[TermAl] Engram source basis for {}: its named source root {root}. Tests count \
                 for it only when they run there{one_call}.",
                entry.short_ref
            )
        }
        None => format!(
            "[TermAl] Engram source basis for {work}: this session's workdir {workdir} (no \
             worktree named). If you do this item's work in another worktree, name it once with \
             termal_name_source_root."
        ),
    }
}

/// A source-root projection retained in historical control cards.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramControlSourceRootCard {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    work_ref: Option<String>,
}

/// The one-call form of `test` as a remedy clause, with the directory left to
/// the agent (`engram_one_call_template` in src/engram_one_call.rs). The test
/// stands alone: one with anything after it is no one-call line, and its exit
/// status could not say it passed. On Windows DIR is written as Windows names
/// it (`ENGRAM_ONE_CALL_DIR_SPELLING`).
fn engram_one_call_clause(test: &str, measured_in: &str) -> String {
    format!(
        "in one call as `pushd \"DIR\" && {test}`, with DIR the absolute directory in \
         {measured_in} where it runs{ENGRAM_ONE_CALL_DIR_SPELLING} and nothing piped, redirected \
         or chained after the test"
    )
}

/// How DIR is to be written in the one-call form, said wherever the form is
/// named: on Windows as Windows names it, since Git Bash's spelling of a drive
/// (`/c/…`), which Claude's Bash tool favours, is one the form does not take
/// (`engram_one_call_git_bash_dir`); nothing on other systems.
const ENGRAM_ONE_CALL_DIR_SPELLING: &str = if cfg!(windows) {
    ", written as a Windows path (C:\\… or C:/…, not Git Bash's /c/…),"
} else {
    ""
};

/// The host line once a recognised test of a turn started in `ran_in`,
/// outside the worktree the turn is measured in (`measured_in`), and so gets
/// no credit whether or not it finishes: that is when a missing name costs
/// something. `template` is the test to name in the one-call form, when the
/// form could carry it (`engram_one_call_template`). Left for a test that gets
/// no check by `engram_uncredited_test_line` in src/engram_one_call.rs.
fn engram_source_root_withheld_line(
    ran_in: &FsPath,
    measured_in: &str,
    named: bool,
    child: bool,
    template: Option<&str>,
) -> String {
    let measured_in = engram_source_root_display(measured_in);
    // A delegated session names no root, so it is told where to run them.
    let remedy = if child {
        "run the tests in your workdir, where your turns are measured".to_owned()
    } else if named {
        match template.map(|test| engram_one_call_clause(test, &measured_in)) {
            Some(clause) => format!(
                "run the tests in that root, {clause}, or name the worktree you work in with \
                 termal_name_source_root"
            ),
            None => "run the tests in that root, or name the worktree you work in with \
                     termal_name_source_root"
                .to_owned(),
        }
    } else {
        "name that worktree once with termal_name_source_root so its tests count".to_owned()
    };
    format!(
        "[TermAl] A recognised test started in {}, outside the worktree this turn is measured in \
         ({measured_in}), and gets no credit: {remedy}.",
        engram_source_root_display(&ran_in.to_string_lossy()),
    )
}

/// Why TermAl cannot confirm that a recognised test ran in the worktree its
/// turn is measured in (`engram_source_root_unconfirmed_line`).
enum EngramUnconfirmedReason<'place> {
    /// The runtime reports no directory, and its shell may be back in this
    /// place (the session's workdir) between calls.
    ShellMayReturn(&'place FsPath),
    /// TermAl cannot place the shell: a directory change it cannot follow,
    /// or another command still changing it.
    ShellUnknown,
    /// The test ran after another command on its line (`A && TEST`), which
    /// TermAl does not read as a shell would (`engram_embedded_test`).
    InsideLine,
    /// The line starts as the one-call form, but with more than one test
    /// alone after its `pushd`: something before or after the test, or a
    /// shell of its own around it.
    OneCallTestNotAlone,
    /// The line starts as the one-call form on Windows but writes its DIR in
    /// Git Bash's spelling of a drive (`/c/…`), which the form does not take
    /// (`engram_one_call_git_bash_dir`); `test_alone`: whether that is all,
    /// or the line also holds more than its one test alone after its `pushd`.
    OneCallGitBashDir { test_alone: bool },
}

/// What TermAl knows of a session and its test that decides which remedy
/// can make the test count (`engram_source_root_unconfirmed_line`); the
/// fourth fact, whether TermAl follows a `cd` into the worktree, it reads
/// from the worktree's path.
#[derive(Clone, Copy, Debug)]
struct EngramRemedyFacts<'test> {
    /// The test, when the one-call form can carry it
    /// (`engram_one_call_template`).
    template: Option<&'test str>,
    /// The runtime reported the directory this command runs in: it starts
    /// each command where it says, so a `cd` in a call of its own does not
    /// carry over.
    reports_directory: bool,
    /// The session's workdir lies in the worktree the turn is measured in, so
    /// a shell that reports no directory counts there once TermAl places it
    /// there too.
    workdir_inside: bool,
}

/// The host line once a recognised test of a turn gets no credit because
/// TermAl cannot confirm it ran in the worktree the turn is measured in
/// (`measured_in`), though its shell was presumed there (`reason`). The
/// remedy follows from four facts (`facts`), so that each is one the session
/// can carry out:
/// - the one-call form, when it can carry the test: it counts wherever the
///   shell is;
/// - else, for a runtime that reports its directory, the test as a line of
///   its own with its working directory where it should run;
/// - else, for a session whose workdir is in the worktree, a `cd` to the
///   directory where the test runs in a call of its own, which TermAl
///   follows, then the test as a line of its own; or a new session, where
///   TermAl follows no `cd` into the worktree's path;
/// - else a session whose workdir is in the worktree.
///
/// A test run after another command on its line may have been meant for
/// another project, so its remedy is offered only for the case it was meant
/// here. Which line a test that gets no check leaves, and with which
/// `reason`, is decided by `engram_uncredited_test_line` in
/// src/engram_one_call.rs.
fn engram_source_root_unconfirmed_line(
    measured_in: &str,
    reason: EngramUnconfirmedReason<'_>,
    facts: EngramRemedyFacts<'_>,
) -> String {
    let measured_in = engram_source_root_display(measured_in);
    let lead = if matches!(
        reason,
        EngramUnconfirmedReason::InsideLine
            | EngramUnconfirmedReason::OneCallTestNotAlone
            | EngramUnconfirmedReason::OneCallGitBashDir { .. }
    ) {
        "If it was meant to count here, run"
    } else {
        "Run"
    };
    let reason = match reason {
        EngramUnconfirmedReason::ShellMayReturn(place) => format!(
            "this runtime reports no directory and its shell may return to {} between calls",
            engram_source_root_display(&place.to_string_lossy())
        ),
        EngramUnconfirmedReason::ShellUnknown => "TermAl cannot tell where its shell is: a \
             directory change it cannot follow, or another command still changing it"
            .to_owned(),
        EngramUnconfirmedReason::InsideLine => {
            "it ran after another command on its line, which TermAl cannot place".to_owned()
        }
        EngramUnconfirmedReason::OneCallTestNotAlone => "its line starts as the one-call form, \
             but the form takes only one test after its `pushd`, alone, with nothing before or \
             after it and no shell of its own"
            .to_owned(),
        EngramUnconfirmedReason::OneCallGitBashDir { test_alone: true } => "its line is the \
             one-call form but for its DIR, written in Git Bash's spelling of a drive (/c/…), \
             which the form does not take, since not every shell reads it as that drive \
             (PowerShell reads it as \\c\\… on the current drive)"
            .to_owned(),
        EngramUnconfirmedReason::OneCallGitBashDir { test_alone: false } => "its line starts \
             as the one-call form, but with its DIR in Git Bash's spelling of a drive (/c/…), \
             which not every shell reads as that drive (PowerShell reads it as \\c\\… on the \
             current drive), and with more than its one test alone after its `pushd`: the form \
             takes neither"
            .to_owned(),
    };
    let remedy = if let Some(test) = facts.template {
        format!("{lead} it {}", engram_one_call_clause(test, &measured_in))
    } else if facts.reports_directory {
        format!(
            "{lead} it as a line of its own, with its working directory set to the directory in \
             {measured_in} where it runs"
        )
    } else if facts.workdir_inside {
        // The shell counts once TermAl places it where the test runs, which
        // a `cd` to an absolute directory it follows does
        // (`engram_shell_move`), whether TermAl lost the shell or not.
        if matches!(
            engram_shell_move(&format!("cd \"{measured_in}\"")),
            EngramShellMove::To(_)
        ) {
            format!(
                "{lead} `cd \"DIR\"` in a call of its own, with DIR the absolute directory in \
                 {measured_in} where it runs, then run it as a line of its own"
            )
        } else {
            format!(
                "{lead} it from a new session whose workdir is in {measured_in}, since TermAl \
                 cannot follow a directory change into that path"
            )
        }
    } else {
        format!("{lead} it from a session whose workdir is in {measured_in}")
    };
    format!(
        "[TermAl] A recognised test gets no credit: TermAl cannot confirm it ran in \
         {measured_in}, where this turn is measured, because {reason}. {remedy}."
    )
}

/// `root` as a delegation takes a cwd: the canonical path without the
/// Windows verbatim prefix `fs::canonicalize` adds.
fn engram_source_root_display(root: &str) -> String {
    match root.strip_prefix(r"\\?\UNC\") {
        Some(share) => format!(r"\\{share}"),
        None => root.strip_prefix(r"\\?\").unwrap_or(root).to_owned(),
    }
}

/// The common Git directory of the worktree whose root is exactly `root`,
/// canonical, read from its own `.git` entry only: a `.git` directory, or a
/// `.git` file naming a linked worktree's directory and its `commondir`. It
/// never looks at an ancestor, so a root whose `.git` is gone does not
/// resolve to the main checkout above it.
fn engram_source_root_common_dir(root: &FsPath) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    let metadata = fs::symlink_metadata(&dot_git).ok()?;
    if metadata.is_dir() {
        return fs::canonicalize(&dot_git).ok();
    }
    if !metadata.is_file() {
        return None;
    }
    let text = fs::read_to_string(&dot_git).ok()?;
    let own = root.join(text.trim().strip_prefix("gitdir:")?.trim());
    // A linked worktree's directory names the common directory; one without
    // `commondir` is not a linked worktree (a submodule's, say).
    let commondir = fs::read_to_string(own.join("commondir")).ok()?;
    fs::canonicalize(own.join(commondir.trim())).ok()
}

/// Whether the worktree at `root` is registered with the repository whose
/// common directory is `common`: it is the main worktree, whose `.git`
/// directory is `common`, or a linked one whose own directory is
/// `<common>/worktrees/<name>` and names `<root>/.git` back in its `gitdir`
/// file. A copied `.git` file, or one whose entry was pruned, fails.
fn engram_source_root_registered(root: &FsPath, common: &FsPath) -> bool {
    let dot_git = root.join(".git");
    let Ok(metadata) = fs::symlink_metadata(&dot_git) else {
        return false;
    };
    if metadata.is_dir() {
        return fs::canonicalize(&dot_git).is_ok_and(|dot_git| dot_git == common);
    }
    let Some(own) = fs::read_to_string(&dot_git)
        .ok()
        .and_then(|text| {
            text.trim()
                .strip_prefix("gitdir:")
                .map(|gitdir| root.join(gitdir.trim()))
        })
        .and_then(|own| fs::canonicalize(own).ok())
    else {
        return false;
    };
    let under_worktrees = own.parent().is_some_and(|parent| {
        parent.file_name().is_some_and(|name| name == "worktrees")
            && parent.parent() == Some(common)
    });
    if !under_worktrees {
        return false;
    }
    let Ok(back) = fs::read_to_string(own.join("gitdir")) else {
        return false;
    };
    let back = PathBuf::from(back.trim());
    let back = if back.is_absolute() {
        back
    } else {
        own.join(back)
    };
    matches!(
        (fs::canonicalize(back), fs::canonicalize(&dot_git)),
        (Ok(back), Ok(dot_git)) if back == dot_git
    )
}

/// Whether the named root is gone: its path no longer exists, or it no
/// longer holds its own `.git` entry, so it is no longer a worktree root.
/// Only a lookup that says so counts (`engram_lookup_says_missing`): a denied or failed one
/// leaves a root that may still exist, and changed, unmeasured.
fn engram_named_root_absent(root: &str) -> bool {
    let path = FsPath::new(root);
    match fs::symlink_metadata(path) {
        Err(error) => engram_lookup_says_missing(error.kind()),
        Ok(_) => fs::symlink_metadata(path.join(".git"))
            .is_err_and(|error| engram_lookup_says_missing(error.kind())),
    }
}

/// Whether a failed file-system lookup says the entry is not there, rather
/// than that it could not be looked at (denied, or an I/O failure).
fn engram_lookup_says_missing(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

/// The source basis at `place` with the committed-input switch given
/// ([`source_revision`]). A root that is not clean while the switch is on has
/// no basis, logged with its paths, as any basis that cannot be taken.
#[cfg(test)]
fn engram_place_source_basis_with(
    place: &EngramBasisPlace,
    ordinary_commit_input: bool,
) -> Option<EngramExecutionSourceBasis> {
    engram_place_source_capture_with(place, ordinary_commit_input).ok().flatten()
}

#[cfg(test)]
fn engram_place_source_capture_with(
    place: &EngramBasisPlace,
    ordinary_commit_input: bool,
) -> EngramSourceCapture {
    match place {
        EngramBasisPlace::Workdir(workdir) => {
            engram_execution_source_capture_with(FsPath::new(workdir), ordinary_commit_input)
        }
        EngramBasisPlace::Named {
            root,
            common_dir_key,
        } => engram_named_root_source_capture_with(root, common_dir_key, ordinary_commit_input),
    }
}

/// The source basis of a named root, taken on exactly `root`: never through
/// the ancestor walk the workdir's basis uses (`engram_worktree_root_path`),
/// so a root that was removed, or lost its `.git`, has no basis rather than
/// the main checkout's. The path must still resolve to itself: one replaced
/// by a link (a symbolic link, or a Windows junction) to another tree of the
/// same repository has no basis, checked before that tree is read and again
/// on the root the capture took. The root must still belong to the repository
/// it was named in (`common_dir_key`), and the historical capture refuses a path
/// that is not its own worktree's root.
/// The historical committed-input switch is explicit ([`source_revision`]):
/// on, a root that is not clean has no basis.
#[cfg(test)]
fn engram_named_root_source_basis_with(
    root: &str,
    common_dir_key: &str,
    ordinary_commit_input: bool,
) -> Option<EngramExecutionSourceBasis> {
    engram_named_root_source_capture_with(root, common_dir_key, ordinary_commit_input).ok().flatten()
}

#[cfg(test)]
fn engram_named_root_source_capture_with(
    root: &str,
    common_dir_key: &str,
    ordinary_commit_input: bool,
) -> EngramSourceCapture {
    let path = FsPath::new(root);
    let resolves_to_itself =
        |resolved: &FsPath| engram_exact_path_key(resolved) == engram_exact_path_key(path);
    match fs::canonicalize(path) {
        Ok(resolved) if resolves_to_itself(&resolved) => {}
        Ok(resolved) => {
            eprintln!(
                "engram> no source basis for the named root {root}: it now leads to {}",
                resolved.display()
            );
            return Ok(None);
        }
        Err(_) => {
            eprintln!("engram> no source basis for the named root {root}: it is gone");
            return Ok(None);
        }
    }
    let Some(common) = engram_source_root_common_dir(path) else {
        eprintln!(
            "engram> no source basis for the named root {root}: it is gone or no longer a \
             worktree root"
        );
        return Ok(None);
    };
    if engram_exact_path_key(&common) != common_dir_key {
        eprintln!(
            "engram> no source basis for the named root {root}: it now belongs to another \
             repository"
        );
        return Ok(None);
    }
    match source_revision(path, ordinary_commit_input) {
        Ok(SourceRevision::Taken {
            root: canonical,
            revision,
        }) if resolves_to_itself(&canonical) => {
            Ok(engram_bounded_source_basis(path, &canonical, revision))
        }
        Ok(SourceRevision::Taken { root: canonical, .. }) => {
            eprintln!(
                "engram> no source basis for the named root {root}: it was measured as {}",
                canonical.display()
            );
            Ok(None)
        }
        Ok(SourceRevision::NotClean { dirty_paths }) => {
            eprintln!(
                "engram> no source basis for the named root {root}: {}",
                source_revision_not_clean_reason(&dirty_paths)
            );
            Ok(None)
        }
        Err(error) => {
            eprintln!("engram> no source basis for the named root {root}: {error:#}");
            match error.downcast_ref::<SourceBasisRefusal>() {
                Some(refusal) => Err(*refusal),
                None => Ok(None),
            }
        }
    }
}

/// Checks that `path` may be a work's source root, for a session working in
/// `workdir` of the project at `project_root`, and returns its canonical root
/// and the key of its repository's common directory. Relative paths resolve
/// from `workdir`. It must be (a) the root of a worktree TermAl can measure
/// (a trusted Git, no configured filters), (b) of the same repository as the
/// workdir, (c) registered with that repository, and (d) inside the project
/// folder, since the acceptance evaluator runs there and a delegated
/// session's folder must lie inside its project. Runs Git: off the lock.
fn validate_engram_source_root(
    path: &str,
    workdir: &str,
    project_root: &str,
) -> std::result::Result<(String, String), String> {
    let network = || {
        format!(
            "`{path}` is on a network share: TermAl measures no source root there (resolving \
             it can block, and the acceptance evaluator cannot start in it); name a local \
             worktree"
        )
    };
    // The path as written first: on Unix a `\\server\share` spelling is a
    // relative name, which would otherwise be joined to the workdir.
    if engram_network_path(path) {
        return Err(network());
    }
    // Git Bash on Windows spells a drive the MSYS way (`/c/github/x`), as
    // its `pwd` prints it; read as written, that would be joined to the
    // workdir.
    let windows_path = engram_msys_drive_path(path);
    let requested = FsPath::new(windows_path.as_ref());
    // `C:wt` on Windows: relative to that drive's current folder, which is
    // the process's and not the session's, so it is not joined to the workdir.
    if !requested.has_root()
        && matches!(
            requested.components().next(),
            Some(std::path::Component::Prefix(_))
        )
    {
        return Err(format!(
            "`{path}` is relative to a drive's current folder; name the worktree by its full \
             path, or relative to this session's folder"
        ));
    }
    let requested = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        FsPath::new(workdir).join(requested)
    };
    if engram_network_path(&requested.to_string_lossy()) {
        return Err(network());
    }
    let project = fs::canonicalize(project_root).map_err(|error| {
        format!("the project folder {project_root} cannot be resolved: {error}")
    })?;
    let outside = |root: &FsPath| {
        format!(
            "`{}` lies outside the project folder {}: the acceptance evaluator runs in the \
             named root, and a delegated session's folder must lie inside its project; create \
             the worktree inside the project (for example under .worktrees/)",
            root.display(),
            project.display()
        )
    };
    // Outside the project before any Git runs there; the root Git resolves
    // is checked again below.
    if let Ok(resolved) = fs::canonicalize(&requested)
        && !resolved.starts_with(&project)
    {
        return Err(outside(&resolved));
    }
    let git = ReviewFreezeGit::new(&requested).map_err(|error| {
        format!("`{path}` is not the root of a worktree TermAl can measure: {error:#}")
    })?;
    let root = git.root.clone();
    // A mapped drive resolves to its share.
    if engram_network_path(&root.to_string_lossy()) {
        return Err(network());
    }
    let common = engram_source_root_common_dir(&root).ok_or_else(|| {
        format!(
            "`{path}` has no Git directory TermAl can read as a worktree's: its `.git` must \
                 be a directory, or a linked worktree's file (the main checkout of a \
                 `--separate-git-dir` repository cannot be named; name a linked worktree of it)"
        )
    })?;
    // Resolve the current directory, including links, before selecting its
    // repository. A missing directory cannot inherit its lexical parent's Git.
    let session_common = fs::canonicalize(workdir).ok()
        .and_then(|directory| test_runs_git_common_dir(&directory))
        .and_then(|common| fs::canonicalize(common).ok())
        .ok_or_else(|| "this session's workdir is not in a Git repository".to_owned())?;
    if engram_exact_path_key(&common) != engram_exact_path_key(&session_common) {
        return Err(format!(
            "`{path}` belongs to another repository than this session's workdir"
        ));
    }
    if !engram_source_root_registered(&root, &common) {
        return Err(format!(
            "`{path}` is not a worktree registered with this repository (a copied or pruned \
             .git entry); create it with git worktree add"
        ));
    }
    if !root.starts_with(&project) {
        return Err(outside(&root));
    }
    Ok((
        root.to_string_lossy().into_owned(),
        engram_exact_path_key(&common),
    ))
}

/// Why a path was not accepted as a source root.
#[derive(Debug, PartialEq, Eq)]
enum EngramSourceRootValidation {
    /// It is not one (`validate_engram_source_root`).
    Refused(String),
    /// Checking it took longer than naming may take.
    OutOfTime(String),
}

/// `validate_engram_source_root` on its own thread, waited for at most
/// `budget`: it resolves paths and reads Git metadata, which a stalled volume
/// can hold for longer than naming may take. Past the budget the name is
/// refused, and the thread is left to finish with its answer dropped.
fn engram_validate_source_root_within(
    live: &Arc<std::sync::atomic::AtomicUsize>,
    lifetime: &Arc<CaptureLifetime>,
    path: String,
    workdir: String,
    project_root: String,
    budget: Duration,
) -> std::result::Result<(String, String), EngramSourceRootValidation> {
    engram_run_source_root_validation_within(
        live,
        lifetime,
        ENGRAM_SOURCE_ROOT_VALIDATION_LIMIT,
        budget,
        path.clone(),
        move || validate_engram_source_root(&path, &workdir, &project_root),
    )
}

/// At most this many path checks run at once, host-wide. A check that
/// outlasted its request keeps its place until its thread ends, so a stalled
/// volume cannot pile up threads call after call.
const ENGRAM_SOURCE_ROOT_VALIDATION_LIMIT: usize = 4;

/// Runs `validate` on its own thread and waits for it at most `budget`, if
/// fewer than `limit` such threads counted in `live` are still running.
fn engram_run_source_root_validation_within(
    live: &Arc<std::sync::atomic::AtomicUsize>,
    lifetime: &Arc<CaptureLifetime>,
    limit: usize,
    budget: Duration,
    path: String,
    validate: impl FnOnce() -> std::result::Result<(String, String), String> + Send + 'static,
) -> std::result::Result<(String, String), EngramSourceRootValidation> {
    use std::sync::atomic::Ordering;
    /// Gives the place back when the thread ends, on a panic too.
    struct Release(Arc<std::sync::atomic::AtomicUsize>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    if live
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |running| {
            (running < limit).then_some(running + 1)
        })
        .is_err()
    {
        return Err(EngramSourceRootValidation::OutOfTime(format!(
            "`{path}` cannot be checked now: earlier path checks are still running (a slow or \
             stalled volume?); name it again later"
        )));
    }
    let release = Release(live.clone());
    let (sender, receiver) = std::sync::mpsc::channel();
    lifetime.spawn("engram-source-root-validate", move || {
            let _release = release;
            let _ = sender.send(validate());
        })
        .map_err(|error| {
            EngramSourceRootValidation::OutOfTime(format!(
                "`{path}` cannot be checked now: {error}; name it again"
            ))
        })?;
    match receiver.recv_timeout(budget) {
        Ok(validated) => validated.map_err(EngramSourceRootValidation::Refused),
        Err(_) => Err(EngramSourceRootValidation::OutOfTime(format!(
            "checking `{path}` took longer than naming may take, so nothing was named; name it \
             again"
        ))),
    }
}

/// The entry naming the source root of `work_id`'s claim `claim_id` in
/// `store`, if any. An entry for another claim of the same work does not
/// apply.
fn engram_work_source_root_for_claim<'a>(
    entries: &'a [EngramWorkSourceRoot],
    store: &EngramAuthorityStoreKey,
    work_id: &str,
    claim_id: &str,
) -> Option<&'a EngramWorkSourceRoot> {
    entries.iter().find(|entry| {
        &entry.store == store && entry.work_id == work_id && entry.claim_id == claim_id
    })
}

/// The entry of `work_id` in `store`, whatever its claim.
fn engram_work_source_root_for_work<'a>(
    entries: &'a [EngramWorkSourceRoot],
    store: &EngramAuthorityStoreKey,
    work_id: &str,
) -> Option<&'a EngramWorkSourceRoot> {
    entries
        .iter()
        .find(|entry| &entry.store == store && entry.work_id == work_id)
}

/// Replaces the entry of `work_id` in `store` with `entry`, or removes it when
/// `entry` is `None`. A new entry on a full list is refused, naming every
/// entry, so the caller can see what holds the list; nothing is evicted.
fn engram_set_work_source_root(
    entries: &mut Vec<EngramWorkSourceRoot>,
    store: &EngramAuthorityStoreKey,
    work_id: &str,
    entry: Option<EngramWorkSourceRoot>,
) -> std::result::Result<(), String> {
    let existing = entries
        .iter()
        .position(|entry| &entry.store == store && entry.work_id == work_id);
    match (existing, entry) {
        (Some(index), Some(entry)) => entries[index] = entry,
        (Some(index), None) => {
            entries.remove(index);
        }
        (None, Some(entry)) => {
            if entries.len() >= ENGRAM_WORK_SOURCE_ROOT_LIMIT {
                let held = entries
                    .iter()
                    .map(|entry| {
                        format!(
                            "{} (named by {} at {}, in store {})",
                            entry.short_ref,
                            entry.named_by_session,
                            entry.named_at,
                            entry.store.project_id
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(format!(
                    "TermAl keeps at most {ENGRAM_WORK_SOURCE_ROOT_LIMIT} named source roots and \
                     evicts none. To free a slot, have a session holding the entry's work claim \
                     explicitly clear that name, or remove the session that named it. A fresh \
                     authoritative read of that claim can also end an obsolete entry. Moving \
                     a session to another claim does not reclaim its old entries: TermAl cannot \
                     query the lifecycle of an arbitrary old claim; its reclamation state is \
                     unknown, never silently ended. An entry in a store \
                     its session's project no longer uses, or of a project with Engram turned \
                     off, cannot be read or cleared and stays until the session that named it \
                     is removed. Entries: {held}"
                ));
            }
            entries.push(entry);
        }
        (None, None) => {}
    }
    Ok(())
}

/// Ends the entries whose naming session no longer exists, so no entry is
/// left that nobody may clear. Returns whether anything ended.
fn engram_end_orphaned_work_source_roots(
    entries: &mut Vec<EngramWorkSourceRoot>,
    session_exists: impl Fn(&str) -> bool,
) -> bool {
    let before = entries.len();
    entries.retain(|entry| session_exists(&entry.named_by_session));
    entries.len() != before
}

/// `termal_name_source_root`: names, or with no `path` clears, the source
/// root of a work the calling session holds a live claim on. `work` is the
/// work id or its short reference.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngramSourceRootRequest {
    work: String,
    /// `None` when omitted, the only way to clear; `Some(None)` for an
    /// explicit null, which is refused rather than read as a clear.
    #[serde(default, deserialize_with = "engram_present_optional_string")]
    path: Option<Option<String>>,
}

/// A field that is present, null or not: `Some` of what it holds, so an
/// explicit null is told apart from an omitted field (`None`, by default).
fn engram_present_optional_string<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramSourceRootResponse {
    work_ref: String,
    work_id: String,
    /// The named root; absent after a clear.
    #[serde(skip_serializing_if = "Option::is_none")]
    root: Option<String>,
    /// Private capture; automatic naming output carries only its summary.
    #[serde(skip)]
    source_basis: Option<EngramExecutionSourceBasis>,
    /// Why the root could not be measured now.
    #[serde(skip_serializing_if = "Option::is_none")]
    unmeasured: Option<String>,
    /// The generation of the new name; 0 after a clear.
    generation: u64,
    /// The previous root, sealed for the caller's turn running in
    /// it under the same claim since before the call; absent when no such
    /// turn took it.
    #[serde(skip_serializing_if = "Option::is_none")]
    sealed: Option<EngramSealedSourceRoot>,
    /// A known limitation of ordinary turns producing an evaluation sighting.
    #[serde(skip_serializing_if = "Option::is_none")]
    evaluation_notice: Option<EngramSourceRootEvaluationNotice>,
    notice: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramSourceRootEvaluationNotice {
    NamedRootSightingRequired,
}

impl EngramSourceRootEvaluationNotice {
    fn message(self) -> &'static str {
        "Acceptance requires an acknowledged matching named-root sighting at the evaluated cut. \
         Naming the source root does not supply that evidence. An existing matching sighting \
         may satisfy acceptance; otherwise a supported measured capture is needed \
         (named_root_sighting_unavailable)."
    }
}

/// Naming is not a source-evidence receipt. This notice does not decide
/// whether Engram already holds a matching sighting at the evaluated cut.
fn engram_source_root_evaluation_notice_locked(
    inner: &StateInner,
    session_id: &str,
) -> Option<EngramSourceRootEvaluationNotice> {
    engram_project_for_session_locked(inner, session_id)
        .and_then(|project| project.engram.as_ref())
        .filter(|settings| settings.is_base_enabled())
        .map(|_| EngramSourceRootEvaluationNotice::NamedRootSightingRequired)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramSealedSourceRoot {
    root: String,
    #[serde(skip)]
    source_id: Option<EngramSourceId>,
}

const ENGRAM_SOURCE_ROOT_TAKES_EFFECT_NOTICE: &str = "The source root takes effect at your next \
     turn; this turn keeps the root it began with.";

const ENGRAM_NAMED_ROOT_SIGHTING_PRECONDITION: &str = "A named root needs an acknowledged capture: \
     an eligible measured execution observation on the claim's run feed at or before the \
     evaluated cut, with source_basis.workspace_id equal to the named root, \
     source_root_generation equal to its generation, and source_root_state = named. A quiet \
     admitted observation with no source change counts; a tracker checkpoint, verification or \
     environment record alone does not.";

/// Added when a root is named: Engram sights a named root only when a turn
/// opens in it, so an evaluation requested in the naming turn is refused
/// (`acceptance_named_root_sighting.rs`).
const ENGRAM_SOURCE_ROOT_EVALUATION_NOTICE: &str = "Acceptance evaluation of this work needs a \
     later turn that opens in the named root: request it from such a turn, not from the turn the \
     root was named in.";

/// Names, or clears, the source root of a work the session in the path holds
/// a live claim on (`AppState::name_engram_source_root`).
async fn name_engram_source_root(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
    request: Result<Json<EngramSourceRootRequest>, JsonRejection>,
) -> Result<Json<EngramSourceRootResponse>, ApiError> {
    let Json(request) =
        request.map_err(|rejection| api_json_rejection("source root request", rejection))?;
    let response =
        run_blocking_api(move || state.name_engram_source_root(&session_id, request)).await?;
    Ok(Json(response))
}

impl AppState {
    /// Takes the lines the prompt of turn `active_turn_generation` carried
    /// out of the pending source-root lines once the runtime accepted it; a
    /// line set since that prompt was built, even one it carried, stays for
    /// the next prompt (`set_pending_source_root_line`).
    fn acknowledge_engram_source_root_line_delivery(
        &self,
        session_id: &str,
        active_turn_generation: u64,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let engram = &mut inner
            .session_mut_by_index(index)
            .expect("session index should be valid")
            .engram;
        if engram
            .source_root_line_delivery
            .as_ref()
            .is_none_or(|(_, turn_generation)| *turn_generation != active_turn_generation)
        {
            return;
        }
        let Some((delivered, _)) = engram.source_root_line_delivery.take() else {
            return;
        };
        let delivered = delivered.lines().collect::<HashSet<_>>();
        engram.pending_source_root_line = engram
            .pending_source_root_line
            .as_deref()
            .map(|pending| {
                pending
                    .lines()
                    .filter(|line| !delivered.contains(line))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|left| !left.is_empty());
        // A delivered repair line no longer has an origin to retire.
        engram.prune_retained_history_repair_origins();
    }

    fn name_engram_source_root(
        &self,
        session_id: &str,
        request: EngramSourceRootRequest,
    ) -> std::result::Result<EngramSourceRootResponse, ApiError> {
        let action = if request.path.is_some() {
            "name"
        } else {
            "clear"
        };
        let work = request.work.clone();
        let path = request.path.clone();
        self.name_engram_source_root_inner(session_id, request)
            .map_err(|mut error| {
                if let Some(deadline) = &mut error.durability_deadline {
                    deadline.identity["action"] = json!(action);
                    deadline.identity["requestedWork"] = json!(work);
                    deadline.identity["requestedPath"] = json!(path);
                    deadline.identity["sessionId"] = json!(session_id);
                }
                error.at_durability_boundary(DurabilityPendingOperation::NamedRootNaming)
            })
    }

    /// Associate the exact held claim through the ordinary work-session child.
    /// Existing control debt and credentials remain historical state; naming
    /// neither replays nor replaces them and creates no turn grant.
    fn bind_fresh_engram_source_root_until(
        &self,
        target: EngramBindingTarget,
        association: &EngramRootIntentAssociation,
        deadline: std::time::Instant,
    ) -> Result<EngramBindingTarget, ApiError> {
        let session_id = &target.connection.session_id;
        let owner = Uuid::new_v4();
        let current = |inner: &StateInner| {
            Self::engram_source_root_target_locked(inner, session_id)
                .ok().flatten().is_some_and(|now| {
                    now.connection == target.connection
                        && now.settings.same_work_service_settings(&target.settings)
                        && now.external_ref == target.external_ref
                })
                && inner.find_session_index(session_id).is_some_and(|index| {
                    Self::engram_root_runtime_is_current(
                        &inner.sessions[index], &target.runtime_snapshot)
                })
        };
        let prior_work = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if !current(&inner) {
                return Err(ApiError::conflict("ordinary naming connection changed before its read"));
            }
            let index = inner.find_session_index(session_id).expect("current session");
            let state = &mut inner.sessions[index].engram;
            if state.source_root_bind_owner.is_some() {
                return Err(ApiError::conflict("source-root naming already has an owner"));
            }
            state.source_root_bind_owner = Some(owner);
            state.work_binding.clone()
        };
        let result = (|| {
            // These reads validate the ordinary child and exact held claim
            // before naming or refreshing that same run's association.
            target.adapter.read_work_session(
                &target.connection, &association.store,
                target.rpc_timeout_until(deadline)
                    .map_err(|error| ApiError::bad_gateway(error.to_string()))?,
            ).map_err(|error| ApiError::bad_gateway(error.to_string()))?;
            let held = target.adapter.read_held_claims(
                &target.connection,
                target.rpc_timeout_until(deadline)
                    .map_err(|error| ApiError::bad_gateway(error.to_string()))?,
            ).map_err(|error| ApiError::bad_gateway(error.to_string()))?;
            // Naming addresses one held claim. A different focused claim is
            // not evidence that this exact association was released.
            let exact = held.items.iter()
                .find(|claim| claim.work_id == association.binding.work_id
                    && claim.claim_id == association.binding.claim_id)
                .and_then(|claim| EngramRootIntentAssociation::validate(
                    &association.store, &claim.work_id, &claim.claim_id,
                    Some(claim.claim_fence), claim.control_binding.as_ref(),
                ).ok());
            if exact.as_ref().is_none_or(|held| held.binding != association.binding) {
                return Err(ApiError::conflict("the exact naming claim is no longer held"));
            }
            // A refused acknowledgement must leave the prior association in
            // place. Reuse naming's bounded retry owner, fenced by the prior
            // association rather than requiring the requested one early.
            self.prepare_engram_authority_retrying_supersession_until(
                &association.store, &association.binding, deadline,
                |inner| current(inner)
                    && inner.find_session_index(session_id).is_some_and(|index| {
                        let state = &inner.sessions[index].engram;
                        state.source_root_bind_owner == Some(owner)
                            && state.work_binding == prior_work
                    }),
            ).map_err(|error| match EngramRootReconcileFailure::Infrastructure(error) {
                EngramRootReconcileFailure::Infrastructure(error)
                    if error.is_enqueued_durability_deadline() => error,
                other => ApiError::bad_gateway(other.into_transport().to_string()),
            })?;
            #[cfg(test)]
            if let Some(meanwhile) =
                TEST_ENGRAM_AFTER_NAMING_BIND_REPLY.with(|hook| hook.borrow_mut().take())
            {
                meanwhile();
            }
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if !current(&inner) {
                return Err(ApiError::conflict("ordinary naming connection changed during its read"));
            }
            let index = inner.find_session_index(session_id).expect("current session");
            let naming_during_turn = matches!(inner.sessions[index].session.status,
                SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping);
            let state = &mut inner.sessions[index].engram;
            if state.source_root_bind_owner != Some(owner) || state.work_binding != prior_work {
                return Err(ApiError::conflict("ordinary naming no longer owns its claim association"));
            }
            // Naming can establish or refresh an association between turns.
            // Another held claim cannot replace a running turn's owner;
            // acknowledged naming selects that claim at the next opening.
            if state.work_binding.as_ref().is_some_and(|binding| {
                binding == &association.binding
                    || (!engram_same_recovery_run(binding, &association.binding)
                        && naming_during_turn)
            }) {
                return Self::engram_source_root_target_locked(&inner, session_id)
                    .map_err(ApiError::conflict)?
                    .ok_or_else(|| ApiError::conflict("ordinary naming connection disappeared"));
            }
            inner.session_mut_by_index(index).expect("current session")
                .engram.work_binding = Some(association.binding.clone());
            #[cfg(test)]
            let save_fails = TEST_ENGRAM_NAMING_BIND_SAVE_FAILS.with(std::cell::Cell::get);
            #[cfg(not(test))]
            let save_fails = false;
            let saved = if save_fails {
                Err(anyhow!("the naming bind's save failed"))
            } else {
                self.commit_session_locked(&mut inner, session_id).map(|_| ())
            };
            if let Err(error) = saved {
                inner.sessions[index].engram.work_binding = prior_work.clone();
                return Err(ApiError::internal(format!("persisting naming binding: {error:#}")));
            }
            Self::engram_source_root_target_locked(&inner, session_id)
                .map_err(ApiError::conflict)?
                .ok_or_else(|| ApiError::conflict("ordinary naming connection disappeared"))
        })();
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if let Some(index) = inner.find_session_index(session_id) {
            let state = &mut inner.sessions[index].engram;
            if state.source_root_bind_owner == Some(owner) {
                state.source_root_bind_owner = None;
            }
        }
        drop(inner);
        self.start_runtime_observation_worker(session_id);
        result
    }
    fn name_engram_source_root_inner(
        &self,
        session_id: &str,
        request: EngramSourceRootRequest,
    ) -> std::result::Result<EngramSourceRootResponse, ApiError> {
        let clock = self.engram_budget_clock();
        // Everything below shares one budget, within the bridge's wait.
        let deadline = clock.now() + ENGRAM_SOURCE_ROOT_NAMING_BUDGET;
        let left = || deadline.saturating_duration_since(clock.now());
        let mut control_left = Duration::from_millis(ENGRAM_MAX_CALL_TIMEOUT_MS);
        let work = request.work.trim().to_owned();
        if work.is_empty() || work.len() > 256 {
            return Err(ApiError::bad_request(
                "`work` must name the work by its id or short reference, in at most 256 bytes",
            ));
        }
        // Only an omitted `path` clears: a blank one is refused, not read as a
        // clear.
        let path = match request
            .path
            .as_ref()
            .map(|path| path.as_deref().map(str::trim))
        {
            None => None,
            Some(None) => {
                return Err(ApiError::bad_request(
                    "`path` is null; name a worktree, or omit `path` to clear the name",
                ));
            }
            Some(Some("")) => {
                return Err(ApiError::bad_request(
                    "`path` is blank; name a worktree, or omit `path` to clear the name",
                ));
            }
            Some(Some(path)) if path.chars().count() > MAX_DELEGATION_CWD_CHARS => {
                return Err(ApiError::bad_request(format!(
                    "`path` must be at most {MAX_DELEGATION_CWD_CHARS} characters"
                )));
            }
            Some(Some(path)) => Some(path.to_owned()),
        };
        let (unfocused_clear, clear_selection, full_new_name, completed_clear) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let store =
                Self::engram_source_root_target_locked(&inner, session_id)
                    .ok()
                    .flatten()
                    .and_then(|target| target.settings.authority_store_key);
            let selection = inner
                .engram_work_source_roots
                .iter()
                .chain(
                    inner
                        .engram_work_naming_history
                        .iter()
                        .flat_map(|history| &history.retirements)
                        .filter(|notice| !notice.published)
                        .map(|notice| &notice.selection),
                )
                .find(|root| {
                    Some(&root.store) == store.as_ref()
                        && (root.work_id == work || root.short_ref == work)
                })
                .cloned();
            let unfocused = path.is_none()
                && selection.as_ref().is_some_and(|root| {
                    inner.find_session_index(session_id).is_some_and(|index| {
                        inner.sessions[index]
                            .engram
                            .work_binding
                            .as_ref()
                            .is_none_or(|binding| {
                                binding.work_id != root.work_id || binding.claim_id != root.claim_id
                            })
                    })
                });
            let full = path.is_some()
                && selection.is_none()
                && engram_named_root_capacity(&inner) >= ENGRAM_WORK_SOURCE_ROOT_LIMIT;
            let completed = path.is_none() && store.as_ref().is_some_and(|store| {
                engram_completed_own_root_clear_locked(&inner, store, session_id, &work).is_some()
            });
            (unfocused, selection, full, completed)
        };
        // An unfocused own-clear is itself the synchronous lifecycle owner.
        // Do not let preparatory maintenance consume its entry first. Current
        // binding/missing-directory reconciliation retains its existing path.
        if !unfocused_clear && !full_new_name && !completed_clear {
            engram_source_root_optional_control_within(&clock, &mut control_left, left(), |budget| {
                self.resolve_removed_engram_roots(session_id, None, budget);
                Ok(())
            })?;
        }
        // `known` fences concurrent selections while the held-claims and
        // lifecycle reads run off the lock.
        let (
            target,
            workdir,
            project_id,
            project_root,
            known,
            validations_live,
            sealable,
            notice_snapshot,
        ) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            // A delegated session by its row or by its own link, as every
            // root-only route tells them.
            if Self::engram_session_has_child_binding_shape_locked(&inner, session_id) {
                return Err(ApiError::conflict(
                    "only a root session holding the work's claim names its source root",
                ));
            }
            let project =
                engram_project_for_session_locked(&inner, session_id).ok_or_else(|| {
                    ApiError::conflict(
                        "this session has no project; Engram is configured per project",
                    )
                })?;
            let project_root = project.root_path.clone();
            // A name associates a claimed worktree with the ordinary service.
            let target =
                Self::engram_source_root_target_locked(&inner, session_id)
                    .map_err(ApiError::conflict)?
                    .ok_or_else(|| {
                        ApiError::conflict(
                            "Engram is not enabled for this session or its project",
                        )
                    })?;
            // Pin the caller's observed turn before off-lock naming. A later
            // turn cannot inherit this turn's measured source endpoint.
            let engram = &inner.sessions[index].engram;
            let sealable = engram.observed_turn_ref().map(str::to_owned).zip(
                engram
                    .observed_source_root()
                    .map(|turn_root| (turn_root.root.clone(), turn_root.claim_id.clone())),
            );
            (
                target,
                inner.sessions[index].session.workdir.clone(),
                inner.sessions[index].session.project_id.clone(),
                project_root,
                inner.engram_work_source_roots.clone(),
                inner.engram_source_root_validations_live.clone(),
                sealable,
                engram.source_root_notices.clone(),
            )
        };
        let store = target.settings.authority_store_key.clone().ok_or_else(|| {
            ApiError::conflict(
                "this project's Engram store has no identity yet; a source root is named after \
                 the session has bound",
            )
        })?;
        if full_new_name {
            // Refusal never spends the caller's naming budget on lifecycle
            // reads. The unconditional host tick remains the retry driver.
            return Err(self.engram_root_capacity_refusal());
        }
        let held =
            engram_source_root_control_within(&clock, &mut control_left, left(), |budget| {
                target
                    .adapter
                    .read_held_claims(
                        &target.connection,
                        ENGRAM_WORK_BINDING_COMMAND_TIMEOUT.min(budget),
                    )
                    .map_err(|error| {
                        ApiError::bad_gateway(format!("engram work core held: {error}"))
                    })
            })?;
        #[cfg(test)]
        if let Some(meanwhile) =
            TEST_ENGRAM_AFTER_SOURCE_ROOT_HELD_READ.with(|hook| hook.borrow_mut().take())
        {
            meanwhile();
        }
        if path.is_none()
            && !held
                .items
                .iter()
                .any(|claim| claim.work_id == work || claim.short_ref == work)
        {
            // Completion has ended the claim. Only its exact acknowledged
            // retirement can answer without entering a new authority transition.
            {
                let inner = self.inner.lock().expect("state mutex poisoned");
                let current = Self::engram_source_root_target_locked(&inner, session_id)
                    .ok().flatten();
                if current.as_ref().is_none_or(|current| {
                    current.connection != target.connection
                        || !current.settings.same_work_service_settings(&target.settings)
                }) {
                    return Err(ApiError::conflict(
                        "Engram project or store changed while the retained root was being cleared",
                    ));
                }
                if let Some(completed) = engram_completed_own_root_clear_locked(
                    &inner, &store, session_id, &work,
                ) {
                    return Ok(completed);
                }
            }
            if let Some(root) = clear_selection.as_ref().filter(|root| root.store == store) {
                if root.named_by_session != session_id {
                    return Err(ApiError::conflict(
                        "not the naming session; another reader cannot confer ownership of this retained name",
                    ));
                }
                return self.clear_obsolete_own_engram_root_until(session_id, root, deadline);
            }
        }
        let claim = held
            .items
            .iter()
            .find(|claim| claim.work_id == work || claim.short_ref == work)
            .cloned()
            .ok_or_else(|| {
                let unlisted = if held.omitted > 0 {
                    format!(
                        " among the {} claims Engram listed ({} more were left out of its list)",
                        held.items.len(),
                        held.omitted
                    )
                } else {
                    String::new()
                };
                ApiError::conflict(format!(
                    "this session holds no live claim on `{work}`{unlisted}; only the agent \
                     holding the claim names its source root"
                ))
            })?;
        if claim.claim_id.is_empty() {
            return Err(ApiError::bad_gateway(
                "this Engram build lists no claim id in `work core held`; install a newer Engram",
            ));
        }
        // An evaluation finds its root by the work's short reference (the
        // show receipt carries no work id), so an entry without one would
        // leave the evaluator in the workdir while the turns use the root.
        if claim.short_ref.is_empty() {
            return Err(ApiError::bad_gateway(
                "Engram listed the claim without its work's short reference in `work core held`, \
                 which TermAl matches evaluations by; install a newer Engram",
            ));
        }
        {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let current =
                Self::engram_source_root_target_locked(&inner, session_id)
                    .ok()
                    .flatten();
            if current.as_ref().is_none_or(|current| {
                current.connection != target.connection
                    || !current.settings.same_work_service_settings(&target.settings)
            }) {
                return Err(ApiError::conflict(
                    "Engram project or store changed while the root was being named",
                ));
            }
        }
        let association = EngramRootIntentAssociation::validate(
            &store,
            &claim.work_id,
            &claim.claim_id,
            Some(claim.claim_fence),
            claim.control_binding.as_ref(),
        )?;
        let target = engram_source_root_control_within(&clock, &mut control_left, left(), |budget| {
            self.bind_fresh_engram_source_root_until(
                target, &association, (clock.now() + budget).min(deadline),
            )
        })?;
        // Read the requested claim's lifecycle without changing session focus.
        // Its exact held association was validated above; the connection can
        // read that run even while the caller remains bound to another claim.
        // A held row alone cannot distinguish recovery from release.
        let root_read_fence =
            EngramRootReadFence::capture(&self.inner.lock().expect("state mutex poisoned"));
        let remote_root =
            engram_source_root_control_within(&clock, &mut control_left, left(), |budget| {
                let phase_deadline = (clock.now() + budget).min(deadline);
                self.guard_engram_root_read_for_naming_classified_until(
                    &target,
                    Some(&association.binding),
                    phase_deadline,
                )
                .map_err(|error| match error {
                    EngramRootReconcileFailure::Infrastructure(error)
                        if error.is_enqueued_durability_deadline() =>
                    {
                        error
                    }
                    other => ApiError::bad_gateway(other.into_transport().to_string()),
                })?;
                self.read_engram_authority_fact_until(
                    &target,
                    &association.binding,
                    phase_deadline,
                )
                .map(|read| Some(read.named_root))
            })?;
        if let Some(intent) = self.engram_orphaned_root_intent(&store, &claim.claim_id) {
            if !target
                .work_binding
                .as_ref()
                .is_some_and(|binding| binding.claim_id == claim.claim_id)
            {
                return Err(ApiError::conflict(
                    "focus this claim and start a turn before recovering its orphaned source-root intent",
                ));
            }
            // A read never acknowledges a possibly executable transport send.
            // Only a covering canonical lifecycle proof settles its exact intent.
            self.reconcile_engram_named_root_with_route_classified_until(
                session_id,
                target.work_binding.as_ref(),
                remote_root.clone(),
                &root_read_fence,
                Some(&target.connection),
                deadline,
            )
            .map_err(|error| {
                let mut error = match error {
                    EngramRootReconcileFailure::Infrastructure(error)
                    | EngramRootReconcileFailure::EvidenceWithheld(
                        EngramRootEvidenceUncertainty::Publication(error)
                        | EngramRootEvidenceUncertainty::CanonicalRead(error)
                        | EngramRootEvidenceUncertainty::CanonicalHistory(error),
                    ) if error.is_enqueued_durability_deadline() => error,
                    other => {
                        return ApiError::bad_gateway(format!(
                            "recovering orphaned root intent: {}",
                            other.into_transport(),
                        ));
                    }
                };
                // Publication can move this exact intent into reconciliation
                // and retire its selection. Retain its immutable generation,
                // not an older confirmed event or a later journal lookup.
                if let Some(deadline) = &mut error.durability_deadline {
                    deadline.identity["generation"] = json!(intent.root.generation);
                }
                error
            })?;
            return Err(ApiError::conflict(
                "the original reporter is unavailable; authoritative recovery was attempted. Retry naming a valid worktree for a fresh generation; evidence stays withheld until it succeeds",
            ));
        }
        // A lost binding reply must remain settleable even if its worktree
        // was removed before retry. The stored absolute path is sufficient
        // to replay the immutable intent; it is never published as a root.
        let missing_pending = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            engram_root_journal(&inner.engram_named_root_journal, &store, &claim.claim_id)
                .and_then(|journal| journal.pending.clone())
        }
        .filter(|event| {
            event.kind == EngramNamedRootKind::Bound
                && path.as_ref().is_some_and(|path| {
                    let path = engram_msys_drive_path(path);
                    let path = FsPath::new(path.as_ref());
                    let absolute = if path.is_absolute() {
                        path.to_path_buf()
                    } else {
                        FsPath::new(&workdir).join(path)
                    };
                    // Settlement replays the stored identity; this comparison
                    // grants no source credit and never changes the intent.
                    engram_canonical_path_key(&absolute)
                        == engram_canonical_path_key(FsPath::new(&event.root.root))
                })
                && engram_named_root_absent(&event.root.root)
        });
        if let Some(event) = missing_pending {
            let receipt =
                engram_source_root_control_within(&clock, &mut control_left, left(), |budget| {
                    self.send_engram_root_event(&target, &event, budget)
                })?;
            self.retire_engram_root_replay(
                &event,
                &receipt.receipt,
                Some(EngramNamedRootEndReason::RootInvalid),
            )?;
            return Err(ApiError::conflict(
                "the pending binding was settled, but its worktree is gone; its root_invalid end is queued for the next turn",
            ));
        }
        let validated = match path {
            None => None,
            Some(path) => Some(
                engram_validate_source_root_within(
                    &validations_live,
                    &self.capture_lifetime,
                    path,
                    workdir.clone(),
                    project_root.clone(),
                    left(),
                )
                .map_err(|refusal| match refusal {
                    EngramSourceRootValidation::Refused(message) => ApiError::bad_request(message),
                    EngramSourceRootValidation::OutOfTime(message) => {
                        ApiError::from_status(StatusCode::SERVICE_UNAVAILABLE, message)
                    }
                })?,
            ),
        };
        // The work's entry as it was before any read began: whatever named
        // the work since (a newer claim's holder, say) makes the name below
        // a conflict rather than be overwritten by what this call read.
        let old = engram_work_source_root_for_work(&known, &store, &claim.work_id).cloned();
        if validated.is_none()
            && matches!(remote_root, Some(EngramNamedRootState::Bound { .. }))
            && old
                .as_ref()
                .is_none_or(|old| old.claim_id != claim.claim_id)
        {
            return Err(ApiError::conflict(
                "Engram has a named root whose original local selection is unavailable; restore or explicitly name this claim's worktree before clearing it",
            ));
        }
        // A rename or a clear seals the old root for the caller's turn running
        // in it on that claim since before this call began (`sealable`;
        // sealed below, under the lock, if that turn still runs); with no
        // such turn nothing is captured. The captures are taken within what
        // is left of the budget, counted with the turns' capture threads; one
        // not taken seals no identity, or reports the new root unmeasured.
        let sealed = old
            .as_ref()
            .filter(|old| validated.as_ref().is_none_or(|(root, _)| *root != old.root))
            .filter(|old| {
                sealable.as_ref().is_some_and(|(_, (root, claim_id))| {
                    *root == old.root && *claim_id == old.claim_id
                })
            })
            .map(|old| EngramSealedSourceRoot {
                root: old.root.clone(),
                source_id: self
                    .engram_naming_source_basis_within(
                        &target, &old.root, &old.common_dir_key,
                        engram_source_root_capture_budget(left(), control_left),
                    )
                    .and_then(|basis| basis.source_id),
            });
        let basis = validated.as_ref().and_then(|(root, common_dir_key)| {
            self.engram_naming_source_basis_within(
                &target, root, common_dir_key,
                engram_source_root_capture_budget(left(), control_left),
            )
        });
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        // No event has been sent yet. A timeout here names nothing; after
        // transport begins, the durable intent carries an unknown outcome.
        if left().is_zero() {
            return Err(ApiError::from_status(
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "naming took longer than its {} s budget, so nothing was named; name it again",
                    ENGRAM_SOURCE_ROOT_NAMING_BUDGET.as_secs()
                ),
            ));
        }
        // The session must still be where the name was checked: the same
        // project, folder and Engram store, with Base MCP on. A project
        // removed or reconfigured meanwhile would leave a name nobody could
        // use or clear.
        let still_there = inner.sessions[index].session.workdir == workdir
            && inner.sessions[index].session.project_id == project_id
            && engram_project_for_session_locked(&inner, session_id)
                .is_some_and(|project| project.root_path == project_root)
            && Self::engram_source_root_target_locked(&inner, session_id)
                .ok()
                .flatten()
                .is_some_and(|current| {
                    current.settings.authority_store_key.as_ref() == Some(&association.store)
                        && current.connection == target.connection
                });
        if !still_there {
            return Err(ApiError::conflict(
                "this session's project, folder or Engram store changed while the root was being \
                 named, so nothing was named; name it again",
            ));
        }
        if engram_work_source_root_for_work(&inner.engram_work_source_roots, &store, &claim.work_id)
            != old.as_ref()
        {
            return Err(ApiError::conflict(
                "the work's source root changed while it was being named; name it again",
            ));
        }
        // Absence from a holder's read may mean handoff. Only Engram's
        // authoritative lifecycle response removes a binding.
        // The same root named again under the same claim is the same name;
        // anything else is a new one, numbered above every name given before.
        let kept = old.as_ref().filter(|old| {
            old.claim_id == claim.claim_id
                && validated.as_ref().is_some_and(|(root, _)| *root == old.root)
                && matches!(&remote_root, Some(EngramNamedRootState::Bound {
                    workspace_id, generation, named_at
                }) if *workspace_id == old.root && u64::try_from(*generation).ok() == Some(old.generation)
                    && engram_named_at_matches(named_at, &old.named_at))
        });
        let pending =
            engram_root_journal(&inner.engram_named_root_journal, &store, &claim.claim_id)
                .and_then(|journal| journal.pending.clone());
        let retrying = pending.is_some();
        if let Some(pending) = &pending {
            if pending.kind == EngramNamedRootKind::Bound
                && !target
                    .work_binding
                    .as_ref()
                    .is_some_and(|binding| binding.claim_id == claim.claim_id)
            {
                return Err(ApiError::conflict(
                    "focus this claim and start a new turn before retrying its uncertain root binding; its authoritative named_root state must be readable",
                ));
            }
            let same = match (&validated, pending.kind) {
                (Some((root, _)), EngramNamedRootKind::Bound) => *root == pending.root.root,
                (None, EngramNamedRootKind::Ended) => {
                    pending.end_reason == Some(EngramNamedRootEndReason::ExplicitClear)
                }
                _ => false,
            };
            if !same {
                return Err(ApiError::conflict(match (pending.kind, pending.end_reason) {
                    (EngramNamedRootKind::Bound, _) => format!(
                        "an earlier named-root transition remains pending for {}; retry that exact naming request first",
                        pending.root.root,
                    ),
                    (EngramNamedRootKind::Ended, Some(EngramNamedRootEndReason::ExplicitClear)) => format!(
                        "an earlier clear of the named root {} remains pending; retry that clear first",
                        pending.root.root,
                    ),
                    (EngramNamedRootKind::Ended, _) => format!(
                        "an earlier end of the named root {}, raised by the host, remains pending, and no call on this claim can retry it yet; naming stays blocked, so report this to the TermAl coordinator",
                        pending.root.root,
                    ),
                }));
            }
        }
        let remote_generation = match &remote_root {
            Some(EngramNamedRootState::Bound { generation, .. }) => *generation,
            Some(EngramNamedRootState::UnboundByRelease {
                last_generation, ..
            }) => *last_generation,
            _ => 0,
        };
        inner.engram_source_root_generation = inner
            .engram_source_root_generation
            .max(u64::try_from(remote_generation).unwrap_or(0));
        let generation = match (&validated, kept) {
            (None, _) => 0,
            (Some(_), _) if pending.is_some() => pending.as_ref().unwrap().root.generation,
            (Some(_), Some(kept)) => kept.generation,
            (Some(_), None) => {
                inner.engram_source_root_generation = inner
                    .engram_source_root_generation
                    .checked_add(1)
                    .filter(|generation| *generation <= i64::MAX as u64)
                    .ok_or_else(|| ApiError::conflict("named-root generation exhausted"))?;
                inner.engram_source_root_generation
            }
        };
        let mut entry = validated
            .as_ref()
            .map(|(root, common_dir_key)| EngramWorkSourceRoot {
                store: store.clone(),
                work_id: claim.work_id.clone(),
                short_ref: claim.short_ref.clone(),
                claim_id: claim.claim_id.clone(),
                claim_fence: claim.claim_fence,
                root: root.clone(),
                common_dir_key: common_dir_key.clone(),
                named_by_session: session_id.to_owned(),
                named_at: kept.map_or_else(
                    || chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    |kept| kept.named_at.clone(),
                ),
                generation,
            });
        if let Some(kept) = kept {
            // Keep the name's identity, but acknowledge the live claim's
            // current fence for new registrations. The original naming event
            // and registrations retain the authority they were recorded with.
            entry = Some(EngramWorkSourceRoot {
                claim_fence: claim.claim_fence,
                ..kept.clone()
            });
        }
        if let Some(pending) = &pending {
            if pending.kind == EngramNamedRootKind::Bound {
                entry = Some(pending.root.clone());
            }
        }
        let event = pending.or_else(|| match (&entry, &old) {
            (Some(entry), _) if kept.is_none() => Some(EngramNamedRootEvent {
                root: entry.clone(), reporter: session_id.to_owned(),
                kind: EngramNamedRootKind::Bound, end_reason: None,
            }),
            (None, Some(old)) if old.claim_id == claim.claim_id && (
                engram_root_journal(&inner.engram_named_root_journal, &store, &claim.claim_id)
                    .is_some_and(|journal| journal.confirmed.is_some())
                || matches!(&remote_root, Some(EngramNamedRootState::Bound { workspace_id, generation, named_at })
                    if *workspace_id == old.root && u64::try_from(*generation).ok() == Some(old.generation) && engram_named_at_matches(named_at, &old.named_at))
            ) => Some(EngramNamedRootEvent {
                root: old.clone(), reporter: old.named_by_session.clone(),
                kind: EngramNamedRootKind::Ended,
                end_reason: Some(EngramNamedRootEndReason::ExplicitClear),
            }),
            _ => None,
        });
        let mut transition = if let Some(event) = event {
            if entry.is_some()
                && old.is_none()
                && engram_named_root_capacity(&inner) >= ENGRAM_WORK_SOURCE_ROOT_LIMIT
            {
                return Err(ApiError::conflict(
                    "named source-root capacity is retained, including unacknowledged retirements; no capacity release is confirmed",
                ));
            }
            let mut trial = inner.engram_work_source_roots.clone();
            engram_set_work_source_root(&mut trial, &store, &claim.work_id, entry.clone())
                .map_err(ApiError::conflict)?;
            // Reserve the immutable intent before releasing the lock. No
            // filesystem capture or control transport runs under this lock.
            self.stage_engram_root_event_locked(&mut inner, &event, Some(&association.binding))?;
            drop(inner);
            let receipt =
                engram_source_root_control_within(&clock, &mut control_left, left(), |budget| {
                    self.send_engram_root_event(&target, &event, budget)
                })?;
            if retrying
                && event.kind == EngramNamedRootKind::Bound
                && !engram_root_event_matches_state(&event, remote_root.as_ref())
            {
                let replay_fence =
                    EngramRootReadFence::capture(&self.inner.lock().expect("state mutex poisoned"));
                let status = engram_source_root_control_within(
                    &clock,
                    &mut control_left,
                    left(),
                    |budget| {
                        self.read_engram_authority_fact_until(
                            &target,
                            &association.binding,
                            (clock.now() + budget).min(deadline),
                        )
                        .map(|read| Some(read.named_root))
                    },
                )?;
                if !engram_root_event_matches_state(&event, status.as_ref()) {
                    if matches!(
                        status,
                        None | Some(EngramNamedRootState::Unknown)
                    ) {
                        return Err(ApiError::bad_gateway(
                            "binding receipt replayed, but this Engram response cannot confirm its lifecycle; pending intent retained",
                        ));
                    }
                    self.retire_engram_root_replay_checked(
                        &event,
                        &receipt.receipt,
                        None,
                        engram_root_retirement_for_state(status.as_ref().unwrap()),
                        Some((&target, &replay_fence, status.as_ref().unwrap())),
                    )?;
                    return Err(ApiError::conflict(
                        "the replayed event is no longer an active named root; name the worktree again to create a fresh generation",
                    ));
                }
            }
            inner = self.inner.lock().expect("state mutex poisoned");
            if engram_work_source_root_for_work(
                &inner.engram_work_source_roots,
                &store,
                &claim.work_id,
            ) != old.as_ref()
                || engram_root_journal(&inner.engram_named_root_journal, &store, &claim.claim_id)
                    .and_then(|journal| journal.pending.as_ref())
                    != Some(&event)
            {
                return Err(ApiError::conflict(
                    "named-root selection changed while its event was in flight; reconcile before retrying",
                ));
            }
            Some((event, receipt))
        } else {
            None
        };
        if let Some((event, reply)) = &mut transition {
            drop(inner);
            #[cfg(test)]
            self.test_engram_authority_ack_boundary("before_naming_selection");
            let (locked, owner, proof) = engram_source_root_control_within(
                &clock, &mut control_left, left(), |budget| {
                    self.learn_engram_naming_authority_until(
                        &target, &target, &association.binding, &reply.owner, &reply.proof, None, (clock.now() + budget).min(deadline),
                    )
                },
            )?;
            inner = locked;
            reply.owner = owner;
            reply.proof = proof;
            if engram_work_source_root_for_work(&inner.engram_work_source_roots, &store, &claim.work_id) != old.as_ref()
                || engram_root_journal(&inner.engram_named_root_journal, &store, &claim.claim_id)
                    .and_then(|journal| journal.pending.as_ref()) != Some(event)
            {
                return Err(ApiError::conflict("named-root selection changed before installation; the exact transition needs reconciliation"));
            }
        }
        let index = inner.find_session_index(session_id).ok_or_else(|| {
            ApiError::conflict("naming session disappeared; transition remains pending")
        })?;
        if inner.sessions[index].session.workdir != workdir
            || inner.sessions[index].session.project_id != project_id
            || !Self::engram_source_root_target_locked(&inner, session_id)
                .ok()
                .flatten()
                .is_some_and(|current| {
                    current.settings.authority_store_key.as_ref() == Some(&store)
                        && current.connection == target.connection
                })
        {
            return Err(ApiError::conflict(
                "the naming session's authority changed; the transition remains pending",
            ));
        }
        let previous = inner.engram_work_source_roots.clone();
        let previous_record = {
            let engram = &inner.sessions[index].engram;
            (
                engram.pending_source_root_line.clone(),
                engram.retained_history_repair_origins.clone(),
                engram.source_root_line_delivery.clone(),
                engram.unadmitted_active_turn.clone(),
                engram.roots_named_in_turn.clone(),
            )
        };
        let line = engram_source_root_line(
            entry.as_ref(),
            &claim.short_ref,
            &workdir,
            engram_one_call_offered_up_front(inner.sessions[index].session.agent),
        );
        if entry.is_some()
            && old.is_none()
            && engram_named_root_capacity(&inner) >= ENGRAM_WORK_SOURCE_ROOT_LIMIT
        {
            return Err(ApiError::conflict(
                "named source-root capacity remains retained; any producer intent is preserved for its existing owner",
            ));
        }
        if let Err(error) = engram_set_work_source_root(
            &mut inner.engram_work_source_roots,
            &store,
            &claim.work_id,
            entry,
        ) {
            // Capacity was checked before transport; a concurrent change
            // can still require a later retry of the retained intent.
            // Never lower the shared allocation watermark after off-lock
            // work: another work may already have allocated above it.
            if inner.engram_work_source_roots != previous
                && self.commit_engram_root_locked(&mut inner).is_err()
            {
                inner.engram_work_source_roots = previous.into();
            }
            return Err(ApiError::conflict(error));
        }
        // A new name given during a turn takes effect at the session's next
        // one (`acceptance_named_root_sighting.rs`). Marked with the turn
        // running as the selection is installed, in this same lock hold and
        // before any later step that may fail or release the lock
        // (persistence, publication): a selection that stays
        // installed always carries its mark, so a retry that keeps the name,
        // and so sends no transition, still finds it. A failed persistence
        // below restores the marks with the selection. A turn admitted later
        // is not taken for the naming turn; one admitted earlier may still
        // capture the new root, and the evaluation check then sees that
        // turn's root and defers to Engram. A kept name changes nothing.
        if let Some((event, _)) = &transition {
            let engram = &mut inner.sessions[index].engram;
            let naming_turn = engram.observed_turn_ref().map(str::to_owned);
            record_engram_root_named_in_turn(engram, &store, event, naming_turn);
        }
        let previous_journal = inner.engram_named_root_journal.clone();
        for journal in &mut inner.engram_named_root_journal {
            if journal.store == store
                && journal.claim_id != claim.claim_id
                && journal.confirmed.as_ref().is_some_and(|(event, _)| {
                    event.root.work_id == claim.work_id && event.kind == EngramNamedRootKind::Bound
                })
            {
                journal.retire(EngramRootRetirement::Displaced);
            }
        }
        // A kept name sends no event, yet Engram's authoritative read above
        // confirmed this very claim's binding at the same generation and
        // named_at, and the work's entry is this claim's. Any displacement or
        // unsettled reconciliation recorded on the claim's own journal is
        // then stale, and would refuse every evaluation under the binding.
        if transition.is_none() && kept.is_some() {
            if let Some(journal) = inner
                .engram_named_root_journal
                .iter_mut()
                .find(|journal| journal.store == store && journal.claim_id == claim.claim_id)
                .filter(|journal| journal.pending.is_none())
            {
                journal.obsolete = false;
                journal.retirement = None;
                if journal
                    .reconciliation
                    .as_ref()
                    .is_some_and(|reconciliation| !reconciliation.settled)
                {
                    journal.reconciliation = None;
                }
            }
        }
        if let Some((event, reply)) = &transition {
            let journal = inner
                .engram_named_root_journal
                .iter_mut()
                .find(|journal| journal.store == store && journal.claim_id == claim.claim_id)
                .expect("staged journal");
            journal.confirmed = Some((event.clone(), reply.receipt.clone()));
            journal.reconciliation = None;
            journal.obsolete = false;
            journal.retirement = None;
            journal.pending = None;
        }
        // The seal is for the caller's turn measured in the old root under
        // the old entry's claim, and is reported only when that turn took it.
        // A turn of another claim in the same tree (two works may name one
        // worktree) is not sealed: its work still names the tree, so its
        // later edits there are its own and a seal would hide them. Nor is a
        // turn started after this call began, whose own start the capture
        // may predate: its reference must match the one `sealable` noted.
        let sealed = match (sealed, &old, &sealable) {
            (Some(sealed), Some(old), Some((turn_ref, _))) => {
                let engram = &mut inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid")
                    .engram;
                let same_turn = engram.observed_turn_ref() == Some(turn_ref.as_str());
                engram
                    .observed_source_root_mut()
                    .filter(|turn_root| {
                        same_turn
                            && turn_root.root == sealed.root
                            && turn_root.claim_id == old.claim_id
                            && turn_root.generation == old.generation
                            && turn_root.common_dir_key == old.common_dir_key
                    })
                    .map(|turn_root| {
                        turn_root.sealed_source_id = sealed.source_id.clone();
                        sealed
                    })
            }
            _ => None,
        };
        if let Err(error) = self.commit_engram_root_locked(&mut inner) {
            // Nothing was kept, so the agent is told of nothing and the
            // running turn keeps its root unsealed.
            inner.engram_work_source_roots = previous.into();
            inner.engram_named_root_journal = previous_journal.into();
            let engram = &mut inner
                .session_mut_by_index(index)
                .expect("session index should be valid")
                .engram;
            // The pending lines and their repair origins come back together.
            (
                engram.pending_source_root_line,
                engram.retained_history_repair_origins,
                engram.source_root_line_delivery,
                engram.unadmitted_active_turn,
                engram.roots_named_in_turn,
            ) = previous_record;
            return Err(ApiError::internal(format!(
                "failed to persist the source root: {error:#}"
            )));
        }
        if let Some((_, reply)) = &transition {
            drop(inner);
            self.publish_engram_authority_until(&store, &reply.owner, deadline)?;
            inner = self.inner.lock().expect("state mutex poisoned");
        } else if let Some(owner) = inner
            .engram_work_naming_history
            .iter()
            .find(|history| history.store == store && history.work_id == claim.work_id)
            .and_then(|history| history.transition.clone())
            .filter(|owner| owner.phase == EngramAuthorityPhase::Prepared)
        {
            let captured = EngramNamingReadContext::capture(&inner, &store, &owner.binding);
            drop(inner);
            #[cfg(test)]
            self.test_engram_authority_ack_boundary("before_kept_naming_proof");
            let proof = self.read_engram_authority_fact_until(&target, &owner.binding, deadline)?;
            let (locked, owner, _) =
                self.learn_engram_naming_authority_until(&target, &target, &association.binding, &owner, &proof, Some(captured), deadline)?;
            inner = locked;
            drop(inner);
            self.publish_engram_authority_until(&store, &owner, deadline)?;
            inner = self.inner.lock().expect("state mutex poisoned");
        }
        let index = inner.find_session_index(session_id).ok_or_else(|| {
            ApiError::conflict("naming session disappeared after persistence acknowledgement")
        })?;
        // An acknowledged explicit clear supersedes only instructions that
        // existed when this request began. Absence or an Ended read alone
        // never means the holder deliberately cleared a name. This changes
        // presentation only; it restores no authority or evidence eligibility.
        if validated.is_none()
            && inner.sessions[index].session.workdir == workdir
            && inner.sessions[index].session.project_id == project_id
            && Self::engram_source_root_target_locked(&inner, session_id)
                .ok()
                .flatten()
                .is_some_and(|current| {
                    current.connection == target.connection
                        && current.settings.same_work_service_settings(&target.settings)
                        && current.work_binding.as_ref().map(|binding| {
                            (
                                &binding.root_execution_id,
                                &binding.work_id,
                                &binding.run_id,
                                &binding.claim_id,
                            )
                        }) == target.work_binding.as_ref().map(|binding| {
                            (
                                &binding.root_execution_id,
                                &binding.work_id,
                                &binding.run_id,
                                &binding.claim_id,
                            )
                        })
                })
            && !engram_authority_work_unresolved(&inner, &store, &claim.work_id)
            && engram_work_source_root_for_work(
                &inner.engram_work_source_roots,
                &store,
                &claim.work_id,
            )
            .is_none()
        {
            let covered: Vec<_> = notice_snapshot
                .into_iter()
                .filter(|notice| {
                    matches!(&notice.kind, EngramSourceRootNoticeKind::SelectionLoss { selection }
                    if selection.store == store && selection.work_id == claim.work_id
                        && selection.claim_id == claim.claim_id)
                })
                .collect();
            acknowledge_engram_source_root_notices_locked(&mut inner.sessions[index], &covered);
        }
        // An acknowledged forward name made the work's retained history
        // current. The repair line this request's own maintenance may have
        // set beforehand no longer holds; the old trace stays uncertain.
        if validated.is_some() {
            retire_engram_retained_history_repair_lines_locked(&mut inner, &store, &claim.work_id);
        }
        // Announce success only after the selection and receipt are durable.
        inner
            .session_mut_by_index(index)
            .expect("session index should be valid")
            .engram
            .set_pending_source_root_line(line);
        // The root image was acknowledged above. Notice delivery is optional
        // metadata and must not resurrect a synchronous authority write here.
        let _ = self.persist_tx.send(PersistRequest::Delta);
        if inner.sessions[index]
            .engram
            .work_binding
            .as_ref()
            .is_some_and(|binding| binding.claim_id == claim.claim_id)
        {
            if let Some((event, _)) = &transition {
                inner.sessions[index].engram.named_root = Some(match event.kind {
                    EngramNamedRootKind::Bound => EngramNamedRootState::Bound {
                        workspace_id: event.root.root.clone(),
                        generation: event.root.generation as i64,
                        named_at: event.root.named_at.clone(),
                    },
                    EngramNamedRootKind::Ended => EngramNamedRootState::None,
                });
            } else if let Some(kept) = kept {
                // A kept name confirms this claim's existing binding without
                // an event. The session may still carry another claim's tuple
                // after a focus change; refresh it after durable acknowledgement.
                inner.sessions[index].engram.named_root = Some(EngramNamedRootState::Bound {
                    workspace_id: kept.root.clone(),
                    generation: kept.generation as i64,
                    named_at: kept.named_at.clone(),
                });
            }
        }
        stage_engram_named_root_selection_locked(
            &mut inner, index, &association.store, &target.connection, &association.binding,
        );
        let (root, source_basis, unmeasured) = match (&validated, basis) {
            (Some((root, _)), Some(basis)) => {
                (Some(root.clone()), Some(basis), None)
            }
            (Some((root, _)), None) => (
                Some(root.clone()),
                None,
                Some("TermAl cannot measure this source root now".to_owned()),
            ),
            (None, _) => (None, None, None),
        };
        let evaluation_notice = root.as_ref().and_then(|_| {
            engram_source_root_evaluation_notice_locked(&inner, session_id)
        });
        let evaluation_message = evaluation_notice.map_or(
            ENGRAM_SOURCE_ROOT_EVALUATION_NOTICE,
            EngramSourceRootEvaluationNotice::message,
        );
        let notice = match &root {
            Some(root) => format!(
                "`{}` now names {root} as its source root. {ENGRAM_SOURCE_ROOT_TAKES_EFFECT_NOTICE} \
                 {ENGRAM_NAMED_ROOT_SIGHTING_PRECONDITION} {evaluation_message}",
                claim.short_ref
            ),
            None => format!(
                "`{}` has no named source root: its turns are measured in the session's workdir. \
                 {ENGRAM_SOURCE_ROOT_TAKES_EFFECT_NOTICE}",
                claim.short_ref
            ),
        };
        // A naming never reports a confirmed root while its work still
        // retains runs no read has answered.
        let notice = match engram_retained_runs_notice_locked(&inner, &store, &claim.work_id) {
            Some(retained) => format!("{notice} {retained}"),
            None => notice,
        };
        Ok(EngramSourceRootResponse {
            work_ref: claim.short_ref,
            work_id: claim.work_id,
            root,
            source_basis,
            unmeasured,
            generation,
            sealed,
            evaluation_notice,
            notice,
        })
    }
}
