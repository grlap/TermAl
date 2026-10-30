// The initial-sighting preflight of an acceptance evaluation. Owns reading
// Engram's `named_root_sighting_read` for the task's run at the task's
// evidence basis, validating that answer, and deciding whether an evaluation
// of a named source root may go ahead: Engram refuses to record an evaluation
// at a cut under a named root that has no sighting there, so an evaluator
// started then would only have its verdict refused. Does not own the rest of
// the request (`acceptance_evaluation_api.rs`), the selection of the
// evaluation's root (`engram_source_roots.rs`), or the final admission, which
// Engram still decides when the evaluation is recorded. The read is a
// prerequisite check only: a present sighting is not a verdict.

/// Engram's answer to `named_root_sighting_read`, schema version 1. Its shape
/// is closed: serde does not refuse unknown fields inside internally tagged
/// variants, so an answer is accepted only when it serializes back to exactly
/// what Engram sent (`parse_engram_named_root_sighting_read`).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct EngramNamedRootSightingRead {
    schema_version: i64,
    project_id: String,
    work_id: String,
    run_id: String,
    read_cut: i64,
    head_cut: i64,
    current_binding: Option<String>,
    binding_changed: bool,
    root: EngramSightingRoot,
}

/// Whether a root is bound at the read cut and, when it is, its sighting.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum EngramSightingRoot {
    None,
    Bound {
        workspace_id: String,
        generation: i64,
        binding_event: String,
        binding_position: i64,
        sighting: EngramRootSighting,
    },
}

/// Whether the bound root has a sighting at or before the read cut.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum EngramRootSighting {
    Absent,
    Present {
        record: String,
        position: i64,
        revision: String,
    },
}

/// The schema version of `named_root_sighting_read` this host understands.
const ENGRAM_NAMED_ROOT_SIGHTING_READ_SCHEMA_VERSION: i64 = 1;

/// The largest answer Engram sends; a larger one is refused, never clipped.
const ENGRAM_NAMED_ROOT_SIGHTING_READ_MAX_BYTES: usize = 16 * 1024;

/// What the read was for: the evaluated work and run, the cut it was taken
/// at, and the local root the evaluation reads (`None`: no root is named
/// locally and the evaluation reads the session's workdir).
struct AcceptanceSightingQuery<'a> {
    work_ref: &'a str,
    project_id: &'a str,
    work_id: &'a str,
    run_id: &'a str,
    cut: i64,
    root: Option<&'a AcceptanceEvaluationSourceRoot>,
}

impl AcceptanceSightingQuery<'_> {
    /// The local root as a refusal names it.
    fn local_root(&self) -> String {
        self.root.map_or_else(
            || "no named source root (it reads the session's workdir)".to_owned(),
            |root| format!("named source root {}", engram_source_root_display(&root.root)),
        )
    }
}

impl EngramNamedRootSightingRead {
    /// Why this answer does not describe `query` coherently, if it does not.
    fn incoherence(&self, query: &AcceptanceSightingQuery<'_>) -> Option<String> {
        if self.schema_version != ENGRAM_NAMED_ROOT_SIGHTING_READ_SCHEMA_VERSION {
            return Some(format!("unsupported schema version {}", self.schema_version));
        }
        if self.project_id != query.project_id
            || self.work_id != query.work_id
            || self.run_id != query.run_id
        {
            return Some("the answer names another project, work or run".to_owned());
        }
        if self.read_cut != query.cut || self.read_cut < 0 || self.head_cut < self.read_cut {
            return Some(format!(
                "the answer was read at cut {} (head {}), not at the requested cut {}",
                self.read_cut, self.head_cut, query.cut
            ));
        }
        if let EngramSightingRoot::Bound {
            generation,
            binding_position,
            sighting,
            ..
        } = &self.root
        {
            let sighted_after_cut = matches!(sighting,
                EngramRootSighting::Present { position, .. } if *position > self.read_cut);
            if *generation < 1
                || *binding_position < 0
                || *binding_position > self.read_cut
                || sighted_after_cut
            {
                return Some("the bound root's positions lie outside the read cut".to_owned());
            }
        }
        None
    }
}

/// `value` as a version-1 sighting answer, or why it is not one: a missing,
/// mistyped or unknown field anywhere in it.
fn parse_engram_named_root_sighting_read(
    value: Value,
) -> std::result::Result<EngramNamedRootSightingRead, String> {
    let read: EngramNamedRootSightingRead = serde_json::from_value(value.clone())
        .map_err(|error| format!("malformed answer: {error}"))?;
    if serde_json::to_value(&read).ok().as_ref() != Some(&value) {
        return Err("the answer has fields this host does not know".to_owned());
    }
    Ok(read)
}

/// The refusal for an evaluation whose root's sighting could not be read or
/// understood: unknown, never taken as absent or present.
fn acceptance_sighting_unknown(query: &AcceptanceSightingQuery<'_>, why: &str) -> ApiError {
    ApiError::conflict(format!(
        "Engram's named-root sighting for `{}` (whose evaluation has {}) could not be \
         determined ({why}); no evaluator was started. Request the evaluation again; if Engram \
         does not support named_root_sighting_read, update the installed Engram first.",
        query.work_ref,
        query.local_root(),
    ))
}

/// The decision for a coherent answer: proceed when neither side binds a root,
/// or when Engram binds the local root, at its generation, with a sighting at
/// the cut.
fn acceptance_sighting_decision(
    read: &EngramNamedRootSightingRead,
    query: &AcceptanceSightingQuery<'_>,
    evaluation_notice: Option<EngramSourceRootEvaluationNotice>,
) -> Result<(), ApiError> {
    if read.binding_changed {
        return Err(ApiError::conflict(format!(
            "Engram's binding of `{}`'s named source root moved after the task's evidence basis \
             (run {}, cut {}); no evaluator was started. Request a new evaluation once the root \
             has settled.",
            query.work_ref, query.run_id, query.cut
        )));
    }
    let (local, workspace_id, generation, sighting) = match (query.root, &read.root) {
        (None, EngramSightingRoot::None) => return Ok(()),
        (Some(_), EngramSightingRoot::None) => {
            return Err(ApiError::conflict(format!(
                "Engram binds no source root for `{}` at the task's evidence basis (run {}, cut \
                 {}), but TermAl evaluates it in its {}; no evaluator was started. Request a new \
                 evaluation once the root has settled.",
                query.work_ref,
                query.run_id,
                query.cut,
                query.local_root()
            )));
        }
        (None, EngramSightingRoot::Bound { workspace_id, .. }) => {
            return Err(ApiError::conflict(format!(
                "Engram binds `{}` to the source root {} at the task's evidence basis (run {}, cut \
                 {}), but TermAl has {}; no evaluator was started. Request a new evaluation once \
                 the root has settled.",
                query.work_ref,
                engram_source_root_display(workspace_id),
                query.run_id,
                query.cut,
                query.local_root()
            )));
        }
        (
            Some(local),
            EngramSightingRoot::Bound {
                workspace_id,
                generation,
                sighting,
                ..
            },
        ) => (local, workspace_id, generation, sighting),
    };
    let root = engram_source_root_display(&local.root);
    if *workspace_id != local.root || u64::try_from(*generation).ok() != Some(local.generation) {
        return Err(ApiError::conflict(format!(
            "Engram binds `{}` to another source root or generation at the task's evidence basis \
             (run {}, cut {}) than TermAl's named root {root} (generation {}); no evaluator was \
             started. Request a new evaluation once the root has settled.",
            query.work_ref, query.run_id, query.cut, local.generation
        )));
    }
    match sighting {
        EngramRootSighting::Present { .. } => Ok(()),
        EngramRootSighting::Absent => {
            let remedy = evaluation_notice.map_or_else(
                || format!(
                    "That root needs an acknowledged capture under `{}`'s claim first: a \
                     mediated turn of the claim's session that opens in the named root after \
                     its naming, with `{}` as its focus, acknowledged by Engram. The turn the \
                     root was named in does not capture it. Then request a new evaluation \
                     from a later turn.",
                    query.work_ref, query.work_ref,
                ),
                |notice| notice.message().to_owned(),
            );
            let error = ApiError::conflict(format!(
                "Engram has no sighting of `{}`'s named source root {root} (generation {}) at the \
                 task's evidence basis (run {}, cut {}), so an evaluation recorded there would be \
                 refused; no evaluator was started. {ENGRAM_NAMED_ROOT_SIGHTING_PRECONDITION} {remedy}",
                query.work_ref,
                local.generation,
                query.run_id,
                query.cut,
            ));
            Err(if evaluation_notice.is_some() {
                error.with_kind(ApiErrorKind::AcceptanceEvaluationNamedRootSightingUnavailable)
            } else {
                error
            })
        }
    }
}

/// A name its session gave during a turn: the name, and the grant of the turn
/// it was given in (`engram_source_roots.rs`). In memory only.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EngramRootNamedInTurn {
    store: EngramAuthorityStoreKey,
    claim_id: String,
    generation: u64,
    grant_id: String,
}

/// Records a named-root transition of the naming session on its marks. Each
/// claim keeps its own mark, replaced or removed by that claim's next
/// transition, so naming or clearing another claim's root leaves it; marks of
/// a turn that has ended are dropped. A Bound transition is marked with
/// `naming_grant`, and only while that turn still runs. The naming path passes
/// the grant active as it installs the selection, under the same lock, so the
/// check against the active grant guards other callers only.
fn record_engram_root_named_in_turn(
    engram: &mut EngramSessionState,
    store: &EngramAuthorityStoreKey,
    event: &EngramNamedRootEvent,
    naming_grant: Option<String>,
) {
    let active = engram.active_grant_id.clone();
    engram.roots_named_in_turn.retain(|named| {
        active.as_ref() == Some(&named.grant_id)
            && !(named.store == *store && named.claim_id == event.root.claim_id)
    });
    if event.kind != EngramNamedRootKind::Bound {
        return;
    }
    if let Some(grant_id) = naming_grant.filter(|grant| active.as_ref() == Some(grant)) {
        engram.roots_named_in_turn.push(EngramRootNamedInTurn {
            store: store.clone(),
            claim_id: event.root.claim_id.clone(),
            generation: event.root.generation,
            grant_id,
        });
    }
}

/// Whether no turn has opened in `root` since it was named: it was named
/// during its naming session's turn that is still that session's turn, and
/// that turn did not open in it. A named root takes effect at the session's
/// next turn (`engram_turn_observations.rs`), and Engram's sighting of it is
/// the capture taken when such a turn opens. Told by turn identity, the grant
/// the naming turn held when the name was committed, never by clock order, and
/// only for a turn-gated session. A turn whose capture took the new root after
/// its begin opened in it, so it is left to Engram, as is every other case: a
/// name given between turns, any later turn whatever its focus, and a name the
/// host no longer remembers after a restart. Only the naming session's turns
/// open in its claim's root: a turn selects the root by the claim it is bound
/// to, and a claim is bound by the one session that holds it.
fn acceptance_named_root_turn_pending_locked(
    inner: &StateInner,
    store: &EngramAuthorityStoreKey,
    root: &AcceptanceEvaluationSourceRoot,
) -> bool {
    let Some(entry) = engram_work_source_root_for_claim(
        &inner.engram_work_source_roots,
        store,
        &root.work_id,
        &root.claim_id,
    )
    .filter(|entry| entry.generation == root.generation && entry.root == root.root) else {
        return false;
    };
    let gated = engram_project_for_session_locked(inner, &entry.named_by_session)
        .and_then(|project| project.engram.as_ref())
        .is_some_and(EngramProjectSettings::is_control_enabled);
    let Some(index) = inner.find_session_index(&entry.named_by_session) else {
        return false;
    };
    if !gated {
        return false;
    }
    let engram = &inner.sessions[index].engram;
    let opened_in_root = engram.active_turn_source_root.as_ref().is_some_and(|turn| {
        turn.root == entry.root
            && turn.claim_id == entry.claim_id
            && turn.generation == entry.generation
    });
    !opened_in_root
        && engram.roots_named_in_turn.iter().any(|named| {
            named.store == *store
                && named.claim_id == entry.claim_id
                && named.generation == entry.generation
                && engram.active_grant_id.as_ref() == Some(&named.grant_id)
        })
}

/// The stable reason code a caller reads in the refusal's text, since an
/// `ApiErrorKind` stays in-process (`wire.rs`).
const ACCEPTANCE_NAMED_ROOT_TURN_PENDING_CODE: &str = "named_root_turn_pending";

/// The refusal for an evaluation requested before any turn opened in the
/// newly named root. Its kind types it for in-process callers and tests; its
/// text carries a stable reason code for remote ones.
fn acceptance_named_root_turn_pending(query: &AcceptanceSightingQuery<'_>) -> ApiError {
    ApiError::conflict(format!(
        "`{}`'s {} was named during a turn of the naming session, and no turn has opened in \
         it since, so Engram has not sighted it; no evaluator was started \
         ({ACCEPTANCE_NAMED_ROOT_TURN_PENDING_CODE}). A named root takes effect at that \
         session's next turn, and Engram sights it when that turn opens in it. Request the \
         evaluation from that next turn or a later one, not from the turn the root was named in.",
        query.work_ref,
        query.local_root(),
    ))
    .with_kind(ApiErrorKind::AcceptanceEvaluationNamedRootTurnPending)
}

impl AppState {
    /// Reads Engram's named-root sighting for the task's run at its evidence
    /// basis, off the state lock, for every evaluation whether or not a root
    /// is named locally (`root`), and refuses before any evaluator or brief
    /// exists unless neither side binds a root, or Engram binds that same root
    /// with a sighting there. A named local root is checked again afterwards:
    /// a rename during the read refuses too. A failed, refused, unsupported or
    /// malformed read is unknown, never taken as absent or present.
    fn require_acceptance_named_root_sighting(
        &self,
        parent_session_id: &str,
        reader: &EngramConnectionConfig,
        store: &EngramAuthorityStoreKey,
        task: &AcceptanceEvaluationTask,
        root: Option<&AcceptanceEvaluationSourceRoot>,
        deadline: std::time::Instant,
        now: &impl Fn() -> std::time::Instant,
    ) -> Result<(), ApiError> {
        let work_id = task
            .canonical_work_id()
            .or(root.map(|root| root.work_id.as_str()))
            .unwrap_or_default();
        let run_id = task
            .canonical_identity
            .as_ref()
            .and_then(|identity| identity.active_run_id.as_deref())
            .or(task.active_run_id.as_deref());
        let query = AcceptanceSightingQuery {
            work_ref: &task.work_ref,
            project_id: &store.project_id,
            work_id,
            run_id: run_id.unwrap_or_default(),
            cut: task.evidence_basis,
            root,
        };
        let Some(run_id) = run_id else {
            return Err(acceptance_sighting_unknown(&query, "the task has no active run"));
        };
        if work_id.is_empty() {
            return Err(acceptance_sighting_unknown(
                &query,
                "the task has no canonical work id",
            ));
        }
        // The read needs no routing token or bound session: it goes to the
        // host reader's control process, the one the request's tracker reads
        // already use, while that reader and store are still the session's.
        let adapter = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let current = acceptance_evaluation_host_target_locked(&inner, parent_session_id)?;
            if current.store != *store || current.connection != *reader {
                return Err(ApiError::conflict(ACCEPTANCE_EVALUATION_STORE_CHANGED_ERROR));
            }
            // Known locally, before the read: a root named during the naming
            // session's current turn has no opening Engram could have sighted.
            if root.is_some_and(|root| acceptance_named_root_turn_pending_locked(&inner, store, root)) {
                return Err(acceptance_named_root_turn_pending(&query));
            }
            inner.engram_host_adapter.clone()
        };
        let budget = deadline
            .saturating_duration_since(now())
            .min(Duration::from_millis(ENGRAM_DEFAULT_CALL_TIMEOUT_MS));
        if budget.is_zero() {
            return Err(acceptance_sighting_unknown(&query, "the request budget was spent"));
        }
        let request = EngramControlRequest::NamedRootSightingRead {
            work_ref: work_id.to_owned(),
            run_id: run_id.to_owned(),
            run_cut: Some(task.evidence_basis),
        };
        let value = adapter
            .request(reader, &request, budget)
            .map_err(|error| acceptance_sighting_unknown(&query, &error.to_string()))?;
        if serde_json::to_vec(&value)
            .map_or(true, |bytes| bytes.len() > ENGRAM_NAMED_ROOT_SIGHTING_READ_MAX_BYTES)
        {
            return Err(acceptance_sighting_unknown(&query, "the answer exceeds 16 KiB"));
        }
        let read = parse_engram_named_root_sighting_read(value)
            .map_err(|why| acceptance_sighting_unknown(&query, &why))?;
        if let Some(why) = read.incoherence(&query) {
            return Err(acceptance_sighting_unknown(&query, &why));
        }
        let inner = self.inner.lock().expect("state mutex poisoned");
        if let Some(root) = root {
            if !root.still_named(&inner.engram_work_source_roots, store) {
                return Err(ApiError::conflict(format!(
                    "`{}`'s named source root changed while its sighting was read; no evaluator was \
                     started. Request a new evaluation.",
                    task.work_ref
                )));
            }
        }
        // The notice is advice about the naming session's current project,
        // not a local substitute for Engram's sighting. Refresh it after the
        // read; in particular, an existing sighting still passes under OFF.
        let evaluation_notice = root.and_then(|root| {
            engram_work_source_root_for_claim(
                &inner.engram_work_source_roots, store, &root.work_id, &root.claim_id,
            )
            .filter(|entry| entry.generation == root.generation && entry.root == root.root)
            .and_then(|entry| {
                engram_source_root_evaluation_notice_locked(&inner, &entry.named_by_session)
            })
        });
        acceptance_sighting_decision(&read, &query, evaluation_notice)?;
        Ok(())
    }
}
