// What a passed batch full gate owes its other members, from its credit to
// Engram's answer for each.
//
// Owns: the pending batch a credited gate leaves until its checkpoint's
// receipt names the gate's own record, and its draining when that checkpoint
// is refused or never confirmed; the owed binds that receipt turns it into,
// persisted before any call; and, for each owed bind, one tick's step under
// the attempt cap: deciding it once, confirming that decision stored before
// it is first sent, sending it, refreshing the session's credentials when
// Engram refuses them, and settling it by Engram's answer. Every member of a
// carried (background or detached) launcher full gate credited as passed gets
// a line about checks' credit (`ENGRAM_CHECK_CREDIT_LINE_PREFIX`); a gate
// refused, dropped or failed before its credit binds nothing, and its own
// no-credit line tells its holder. A gate run in the foreground is credited to
// its own item only and records no members.
//
// Does not own: which members a gate has, whether one may be bound, the
// request's bytes, or how Engram's answer classifies
// (`engram_verification_bind.rs`); the checkpoint, its report and its receipt
// (`engram_host_adapter.rs`); the thread the ticks run on
// (`engram_retry_schedule.rs`); or persisting the session record
// (`persisted_state.rs`, `persist_fence.rs`).
//
// A new module, included from main.rs beside `engram_verification_bind.rs`.

/// The most times one member is sent before it settles as unconfirmed.
const ENGRAM_OWED_BIND_MAX_ATTEMPTS: u32 = 8;

/// The longest one member stays owed, or one batch pending its checkpoint's
/// answer, before it settles with a line.
const ENGRAM_OWED_BIND_MAX_MINUTES: i64 = 30;

/// The wait after each attempt that settled nothing, in seconds; the last
/// repeats. Eight attempts span about sixteen minutes, inside the bound.
const ENGRAM_OWED_BIND_DELAYS_SECONDS: [i64; 7] = [5, 15, 30, 60, 120, 240, 480];

/// The longest one tick of owed binds spends on Engram calls; what it does
/// not reach waits for the next tick. Only the production tick thread uses
/// it; tests pass their own budget.
#[cfg_attr(test, allow(dead_code))]
const ENGRAM_OWED_BIND_TICK_BUDGET: Duration = Duration::from_secs(20);

/// The least time one decision's store confirmation is given, whatever the
/// tick's Engram calls left: the writer can be slow under load. Unconfirmed
/// within it, the decision stays owed and a later tick confirms it again.
const ENGRAM_OWED_BIND_CONFIRM_FLOOR: Duration = Duration::from_secs(5);

/// A passed gate with members, credited into a checkpoint that has not
/// answered yet. Exact immutable-original receipt inputs survive restart and
/// wait for that original's result. Other interrupted launch credit is lost.
/// Refusal and expiry drain only inputs without such canonical custody.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramPendingBatch {
    /// The grant of the checkpoint that reports the gate.
    checkpoint_grant_id: String,
    /// The id of the gate's producer observation in that report
    /// (`engram_batch_producer_observation_id`).
    producer_observation_id: String,
    measurement: EngramBindMeasurement,
    targets: Vec<EngramBindTarget>,
    /// When the gate was credited (RFC 3339).
    #[serde(default)]
    pended_at: String,
}

// This is persisted receipt input, not restored launch credit. Only the exact
// successful producer and Test input in an immutable original can retain it.
fn checkpoint_pending_batch_matches_original(batch: &EngramPendingBatch,
    operation: &EngramCheckpointOperation) -> bool {
    let EngramControlRequest::TurnCheckpoint { grant_id, report, .. } = &operation.original.request
        else { return false };
    if batch.checkpoint_grant_id != *grant_id
        || operation.original.settings.authority_store_key.as_ref() != Some(&operation.protection.authority)
        || operation.original.work_binding.is_none()
        || engram_parse_time(&batch.pended_at).is_none()
        || engram_parse_time(&batch.measurement.measured_at).is_none()
        || batch.targets.is_empty() {
        return false;
    }
    let mut producers = report.observations.iter()
        .filter(|producer| producer.observation_id == batch.producer_observation_id);
    let Some(producer) = producers.next() else { return false };
    if producers.next().is_some() || producer.outcome != EngramExecutionOutcome::Succeeded {
        return false;
    }
    let Some(source) = &producer.source_basis else { return false };
    if source.workspace_id != batch.measurement.workspace_id
        || source.source_revision != batch.measurement.source_revision
        || producer.observed_at.as_deref().and_then(engram_parse_time)
            != engram_parse_time(&batch.measurement.measured_at) {
        return false;
    }
    let inputs = report.verification_evidence.iter().filter(|input| {
        let EngramObservationReference::ObservationId { observation_id } = &input.producer_observation;
        input.check_kind == EngramVerificationKind::Test && observation_id == &batch.producer_observation_id
    }).count();
    let mut targets = HashSet::new();
    inputs == 1 && batch.targets.iter().all(|target| {
        !target.work_id.trim().is_empty() && !target.claim_id.trim().is_empty()
            && !target.short_ref.trim().is_empty() && target.claim_fence > 0 && target.root_generation > 0
            && targets.insert(&target.work_id)
    })
}

fn pending_checkpoint_batch_owner<'a>(inner: &'a StateInner, session_id: &str,
    batch: &EngramPendingBatch) -> Option<&'a EngramCheckpointOperation> {
    let record = &inner.sessions[inner.find_session_index(session_id)?];
    let project = record.session.project_id.as_deref().and_then(|id| inner.find_project(id))
        .or_else(|| engram_project_for_session_locked(inner, session_id))?;
    let mut candidates = inner.engram_session_obligations.checkpoint_operations.iter().filter(|operation| {
        operation.protection.local_session_id == session_id
            && operation.protection.project_id == project.id
            && std::path::Path::new(&project.root_path) == operation.original.connection.project_root
            && operation.validate().is_ok()
            && (operation.has_uncertain_checkpoint_history()
                || matches!(operation.progress, EngramExistingCheckpointProgress::Accepted { .. }
                    | EngramExistingCheckpointProgress::Retained)
                || operation.attempts.last().is_some_and(|attempt| attempt.result.is_none()))
            && record.engram.checkpoint_operation.as_ref().is_none_or(|mirror|
                serde_json::to_value(mirror).ok() == serde_json::to_value(operation).ok())
            && checkpoint_pending_batch_matches_original(batch, operation)
    });
    let operation = candidates.next()?;
    candidates.next().is_none().then_some(operation)
}

fn retained_checkpoint_pending_batches(inner: &StateInner, session_id: &str) -> Vec<EngramPendingBatch> {
    let Some(index) = inner.find_session_index(session_id) else { return Vec::new() };
    inner.sessions[index].engram.pending_batches.iter()
        .filter(|batch| pending_checkpoint_batch_owner(inner, session_id, batch).is_some())
        .cloned().collect()
}

// These inputs live in the session row, not in the canonical report. An
// administrative action cannot remove that row or its project context before
// the existing batch owner has converted and settled them.
fn engram_session_has_checkpoint_batch_custody(inner: &StateInner, session_id: &str) -> bool {
    inner.find_session_index(session_id).is_some_and(|index| {
        !inner.sessions[index].engram.owed_binds.is_empty()
            || inner.sessions[index].engram.pending_batches.iter()
                .any(|batch| pending_checkpoint_batch_owner(inner, session_id, batch).is_some())
    })
}

fn engram_project_has_checkpoint_batch_custody(inner: &StateInner, project_id: &str) -> bool {
    inner.sessions.iter().any(|record| {
        (record.session.project_id.as_deref() == Some(project_id)
            || engram_project_for_session_locked(inner, &record.session.id)
                .is_some_and(|project| project.id == project_id))
            && engram_session_has_checkpoint_batch_custody(inner, &record.session.id)
    })
}

/// The members of a full gate launched in `root` by the session at `index`,
/// bound to its claim: the other items it named on the same worktree
/// (`engram_bind_targets_at_launch`), each with the fence its claim has in
/// `held`, the session's held claims read at the launch. With no read
/// (`None`), the members only, with their root entries' fences: a launch
/// whose read failed binds none of them. None when the session is unbound,
/// its claim names no root, or the gate ran outside that root.
fn engram_launch_bind_targets(
    inner: &StateInner,
    index: usize,
    session_id: &str,
    root: &FsPath,
    held: Option<&EngramHeldClaims>,
) -> Vec<EngramBindTarget> {
    let Some(store) = engram_project_for_session_locked(inner, session_id)
        .and_then(|project| project.engram.as_ref())
        .and_then(|settings| settings.authority_store_key.as_ref())
    else {
        return Vec::new();
    };
    let Some(binding) = inner.sessions[index].engram.work_binding.as_ref() else {
        return Vec::new();
    };
    let Some(launch) = engram_work_source_root_for_claim(
        &inner.engram_work_source_roots,
        store,
        &binding.work_id,
        &binding.claim_id,
    ) else {
        return Vec::new();
    };
    if engram_path_key(FsPath::new(&launch.root)) != engram_path_key(root) {
        return Vec::new();
    }
    let unread = EngramHeldClaims {
        items: Vec::new(),
        omitted: 0,
    };
    engram_bind_targets_at_launch(
        &inner.engram_work_source_roots,
        launch,
        session_id,
        held.unwrap_or(&unread),
    )
}

/// Whether the command whose check `check` ends with `exit` launches a gate
/// that is carried and has batch members, so the session's held claims are
/// read at the launch (`engram_launch_batch_members`).
fn engram_launch_needs_held_read(
    inner: &StateInner,
    index: usize,
    session_id: &str,
    check: &EngramTurnCheck,
    exit: Option<EngramCommandExit>,
) -> bool {
    engram_check_is_launcher_full(&check.command)
        && matches!(
            engram_launch_disposition(&check.command, exit.unwrap_or(EngramCommandExit::Unknown)),
            EngramLaunchDisposition::Carry
        )
        && !engram_launch_bind_targets(inner, index, session_id, &check.target.root, None)
            .is_empty()
}

/// The members a gate launched at `check` records, each with the fence its
/// claim has in `launch_held`, the session's held claims read at the launch,
/// and a line for each member whose fence at launch is not known: every
/// member when the read failed or was not made, and one missing from a list
/// Engram cut short (`omitted`), which may still be held. A member missing
/// from a complete list is not held: it keeps its root entry's fence and
/// settlement tells its holder.
fn engram_launch_batch_members(
    inner: &StateInner,
    index: usize,
    session_id: &str,
    check: &EngramTurnCheck,
    launch_held: Option<&std::result::Result<EngramHeldClaims, EngramTransportError>>,
) -> (Vec<EngramBindTarget>, Vec<String>) {
    let members = engram_launch_bind_targets(inner, index, session_id, &check.target.root, None);
    let unknown = |target: &EngramBindTarget, why: &str| {
        engram_batch_not_recorded_line(
            &target.short_ref,
            &format!("its claim's fence at the batch gate's launch {why}"),
        )
    };
    let Some(Ok(held)) = launch_held else {
        let lines = members.iter().map(|target| unknown(target, "could not be read")).collect();
        return (Vec::new(), lines);
    };
    let mut targets = Vec::new();
    let mut lines = Vec::new();
    for mut target in members {
        match held
            .items
            .iter()
            .find(|claim| claim.work_id == target.work_id && claim.claim_id == target.claim_id)
        {
            Some(claim) => {
                target.claim_fence = claim
                    .control_binding
                    .as_ref()
                    .map_or(claim.claim_fence, |binding| binding.claim_fence);
                targets.push(target);
            }
            None if held.omitted > 0 => lines.push(unknown(
                &target,
                "is not known: Engram's list of held claims was cut short",
            )),
            None => targets.push(target),
        }
    }
    (targets, lines)
}

/// The line the holder of a batch gate's member `short_ref` gets when a
/// restart came `when`: the gate's record can no longer be bound to it. Every
/// member is in its session's persisted record as a carried gate's member, a
/// pending batch's or an owed bind, from the gate's launch until it settles
/// with a line, so a restart never leaves one untold.
fn engram_batch_member_lost_line(short_ref: &str, when: &str) -> String {
    engram_batch_not_recorded_line(
        short_ref,
        &format!(
            "TermAl restarted {when}, so the batch gate's record could not be bound to it; run \
             this item's gate again"
        ),
    )
}

/// Takes from `pending` every batch `drained` selects, with a line per member
/// saying `why` its gate's record is not bound to it.
fn engram_drain_pending_batches(
    pending: &mut Vec<EngramPendingBatch>,
    drained: impl Fn(&EngramPendingBatch) -> bool,
    why: &str,
) -> Vec<String> {
    let (taken, kept): (Vec<_>, Vec<_>) = std::mem::take(pending).into_iter().partition(drained);
    *pending = kept;
    taken
        .iter()
        .flat_map(|batch| batch.targets.iter())
        .map(|target| engram_batch_not_recorded_line(&target.short_ref, why))
        .collect()
}

/// Whether `batch` has waited for its checkpoint's answer past
/// `ENGRAM_OWED_BIND_MAX_MINUTES` at `now`, or since a time it cannot say.
fn engram_pending_batch_expired(batch: &EngramPendingBatch, now: chrono::DateTime<chrono::Utc>) -> bool {
    engram_parse_time(&batch.pended_at).is_none_or(|pended| {
        now - pended > chrono::Duration::minutes(ENGRAM_OWED_BIND_MAX_MINUTES)
    })
}

/// The id a turn report gives the producer observation of one check: the
/// same digest `engram_turn_report` and the test-run evidence association
/// build, from the session and the grant, sequence and key the check ran
/// under (for a carried gate, those of its launch).
fn engram_batch_producer_observation_id(
    session_id: &str,
    grant_id: &str,
    sequence: impl std::fmt::Display,
    key: &str,
) -> String {
    sha256_hex(format!("termal-turn-check:{session_id}:{grant_id}:{sequence}:{key}").as_bytes())
}

/// What a member was found to be bindable with: kept so every send rebuilds
/// the same request, with only the routing token current.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramOwedBindDecided {
    binding: EngramControlWorkBinding,
    sighting: EngramBindSighting,
}

/// One member a passed gate's own record is still to be bound to. It is
/// persisted with its session before the first call, so a restart sends it
/// again: Engram replays a committed key and never binds twice.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramOwedBind {
    /// The gate's own passed record.
    original: String,
    measurement: EngramBindMeasurement,
    target: EngramBindTarget,
    /// Set once the member was found bindable; never decided again after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decided: Option<EngramOwedBindDecided>,
    /// The decision was made in this process and is not confirmed stored
    /// yet. It is never sent before it is: a restart after a send must find
    /// the same decision, so it sends the same request rather than deciding
    /// again. One read back from the store is stored, so it loads `false`.
    #[serde(skip)]
    decision_unconfirmed: bool,
    #[serde(default)]
    attempts: u32,
    first_owed_at: String,
    /// What the last attempt that settled nothing said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_detail: Option<String>,
    /// Not tried again before this time (RFC 3339), after an attempt that
    /// settled nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_attempt_at: Option<String>,
}

impl EngramOwedBind {
    fn key(&self) -> (String, String) {
        (self.original.clone(), self.target.work_id.clone())
    }
}

/// What a checkpoint's answer leaves for the pending batches of its grant.
#[derive(Debug, Default, PartialEq, Eq)]
struct EngramBatchReceiptSettlement {
    owed: Vec<EngramOwedBind>,
    /// Lines for members that can now never be bound.
    lines: Vec<String>,
}

/// Takes from `pending` the batches of the checkpoint `receipt` answered and
/// turns each into owed binds, one per member, with the gate's own record as
/// their original. That record is the receipt's id at the index of the gate's
/// own Test input in `report`: Engram returns one id per input, in input order
/// (a contract Engram pins; `settle_test_run_verification_receipt` relies on it
/// too). A receipt that does not name every input once, with valid ids, owes
/// nothing, and the members' holder is told.
fn engram_owe_batch_binds(
    pending: &mut Vec<EngramPendingBatch>,
    report: &EngramTurnReport,
    receipt: &EngramCheckpointReceipt,
    now: &str,
) -> EngramBatchReceiptSettlement {
    let (answered, kept): (Vec<_>, Vec<_>) = std::mem::take(pending)
        .into_iter()
        .partition(|batch| batch.checkpoint_grant_id == receipt.grant_id);
    *pending = kept;
    let mut settlement = EngramBatchReceiptSettlement::default();
    // Engram mints a verification record's id as 32 hex digits (64 for ids
    // stored before that): `is_engram_record_id` accepts both.
    let ids_valid = receipt.verification_evidence.len() == report.verification_evidence.len()
        && receipt
            .verification_evidence
            .iter()
            .all(|id| is_engram_record_id(id))
        && receipt
            .verification_evidence
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            == receipt.verification_evidence.len();
    for batch in answered {
        let original = ids_valid
            .then(|| {
                report
                    .verification_evidence
                    .iter()
                    .zip(&receipt.verification_evidence)
                    .find(|(input, _)| {
                        let EngramObservationReference::ObservationId { observation_id } =
                            &input.producer_observation;
                        input.check_kind == EngramVerificationKind::Test
                            && *observation_id == batch.producer_observation_id
                            && report.observations.iter().any(|producer| {
                                producer.observation_id == *observation_id
                                    && producer.outcome == EngramExecutionOutcome::Succeeded
                            })
                    })
                    .map(|(_, id)| id.clone())
            })
            .flatten();
        match original {
            Some(original) => settlement.owed.extend(batch.targets.into_iter().map(|target| {
                EngramOwedBind {
                    original: original.clone(),
                    measurement: batch.measurement.clone(),
                    target,
                    decided: None,
                    decision_unconfirmed: false,
                    attempts: 0,
                    first_owed_at: now.to_owned(),
                    last_detail: None,
                    next_attempt_at: None,
                }
            })),
            None => settlement.lines.extend(batch.targets.iter().map(|target| {
                engram_batch_not_recorded_line(
                    &target.short_ref,
                    "Engram's checkpoint answer does not name the gate's own passed record",
                )
            })),
        }
    }
    settlement
}

/// What one tick does with an owed bind.
#[derive(Debug, PartialEq, Eq)]
enum EngramOwedBindStep {
    /// Its next attempt is not due yet.
    Wait,
    /// Read the member's claim and newest sighting, then decide.
    Decide,
    /// Its decision is not confirmed stored yet: confirm it, never send it.
    Confirm,
    /// Send the request built from what was decided.
    Send,
    /// It was sent or tried too often or too long: settle it with this line.
    GiveUp(String),
}

/// The step `owed` takes at `now`. Past `ENGRAM_OWED_BIND_MAX_ATTEMPTS`
/// attempts or `ENGRAM_OWED_BIND_MAX_MINUTES` since it was owed, it gives up
/// with its last detail, so a member that never gets a certain answer still
/// gets a line. A decision is sent only once it is confirmed stored.
fn engram_owed_bind_step(
    owed: &EngramOwedBind,
    now: chrono::DateTime<chrono::Utc>,
) -> EngramOwedBindStep {
    let owed_for = engram_parse_time(&owed.first_owed_at).map(|first| now - first);
    let too_long = owed_for
        .is_none_or(|owed_for| owed_for > chrono::Duration::minutes(ENGRAM_OWED_BIND_MAX_MINUTES));
    if owed.attempts >= ENGRAM_OWED_BIND_MAX_ATTEMPTS || too_long {
        let detail = owed.last_detail.as_deref().unwrap_or("no answer settled it");
        return EngramOwedBindStep::GiveUp(if engram_owed_bind_may_be_bound(owed) {
            engram_batch_unconfirmed_line(
                &owed.target.short_ref,
                &format!("no certain answer after {} attempts ({detail})", owed.attempts),
            )
        } else {
            engram_batch_not_recorded_line(
                &owed.target.short_ref,
                &format!("its bind was never sent ({detail}); run this item's gate again"),
            )
        });
    }
    if owed.decided.is_some() && owed.decision_unconfirmed {
        return EngramOwedBindStep::Confirm;
    }
    if owed
        .next_attempt_at
        .as_deref()
        .and_then(engram_parse_time)
        .is_some_and(|due| now < due)
    {
        return EngramOwedBindStep::Wait;
    }
    if owed.decided.is_some() {
        EngramOwedBindStep::Send
    } else {
        EngramOwedBindStep::Decide
    }
}

/// Whether Engram may hold `owed`'s bind. A decision is confirmed stored
/// before its first send, and a send's outcome may be stored only after a
/// crash took it, so every decided entry whose decision is confirmed may
/// have been sent and bound (one read back from the store always is). One
/// never decided, or decided here and not confirmed yet, was certainly never
/// sent.
fn engram_owed_bind_may_be_bound(owed: &EngramOwedBind) -> bool {
    owed.decided.is_some() && !owed.decision_unconfirmed
}

/// `owed` as loaded after a restart at `now`: owed afresh from the load, with
/// at least one attempt left and none waiting, so it is sent again and
/// Engram's replay by its key settles it, however long the host was down.
fn engram_owed_bind_after_load(mut owed: EngramOwedBind, now: &str) -> EngramOwedBind {
    owed.first_owed_at = now.to_owned();
    owed.next_attempt_at = None;
    owed.attempts = owed.attempts.min(ENGRAM_OWED_BIND_MAX_ATTEMPTS - 1);
    owed
}

/// The line telling a batch gate's member `short_ref`'s holder that whether
/// the gate's record is bound to it is not known, and `why`.
fn engram_batch_unconfirmed_line(short_ref: &str, why: &str) -> String {
    format!(
        "{ENGRAM_CHECK_CREDIT_LINE_PREFIX} the full gate is unconfirmed for {short_ref}: {why}; \
         it may or may not be bound."
    )
}

/// The wait between asks for fresh credentials, in seconds.
const ENGRAM_OWED_BIND_CREDENTIAL_WAIT_SECONDS: i64 = 30;

/// Puts `owed` off while the session waits for fresh credentials at `now`,
/// counting no attempt: nothing was sent, so a bind owed after a restart
/// keeps the attempt it was given, and only the time cap bounds the wait.
fn engram_owed_bind_wait_for_credentials(owed: &mut EngramOwedBind, now: chrono::DateTime<chrono::Utc>) {
    owed.last_detail = Some("waiting for fresh Engram credentials".to_owned());
    owed.next_attempt_at = Some(
        (now + chrono::Duration::seconds(ENGRAM_OWED_BIND_CREDENTIAL_WAIT_SECONDS))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    );
}

/// Counts an attempt on `owed` at `now` that settled nothing, keeping what it
/// said, and puts its next attempt off by the schedule.
fn engram_owed_bind_attempt_failed(
    owed: &mut EngramOwedBind,
    detail: String,
    now: chrono::DateTime<chrono::Utc>,
) {
    owed.attempts = owed.attempts.saturating_add(1);
    owed.last_detail = Some(detail);
    let index = usize::try_from(owed.attempts.saturating_sub(1))
        .unwrap_or(usize::MAX)
        .min(ENGRAM_OWED_BIND_DELAYS_SECONDS.len() - 1);
    owed.next_attempt_at = Some(
        (now + chrono::Duration::seconds(ENGRAM_OWED_BIND_DELAYS_SECONDS[index]))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    );
}

/// Settles `owed` by Engram's answer at `now`: the line its holder is told
/// when the answer settles it, or `None` when it stays owed, its attempt
/// counted.
fn engram_owed_bind_answered(
    owed: &mut EngramOwedBind,
    outcome: EngramBindOutcome,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    // A collision's line also names the rejected bind's target provenance,
    // so the durable disclosure keeps its original identity.
    let outcome = match outcome {
        EngramBindOutcome::Collision(detail) => EngramBindOutcome::Collision(format!(
            "{detail}; claim {} fence {} root generation {}",
            owed.target.claim_id, owed.target.claim_fence, owed.target.root_generation
        )),
        other => other,
    };
    match engram_bind_notice_line(&owed.target.short_ref, &outcome) {
        Some(line) => Some(line),
        None => {
            let detail = match outcome {
                EngramBindOutcome::Resend(detail) | EngramBindOutcome::Refresh(detail) => detail,
                _ => "no answer settled it".to_owned(),
            };
            engram_owed_bind_attempt_failed(owed, detail, now);
            None
        }
    }
}

/// The request for `owed` under the session's current `routing_token`, or
/// `None` before it was decided. Every send of one owed bind rebuilds the
/// same request: only the routing token, which is not part of the intent
/// Engram compares, can differ.
fn engram_owed_bind_request(owed: &EngramOwedBind, routing_token: &str) -> Option<EngramControlRequest> {
    let decided = owed.decided.as_ref()?;
    Some(engram_verification_bind_request(
        routing_token,
        &owed.original,
        owed.measurement.clone(),
        decided.binding.clone(),
        decided.sighting.clone(),
    ))
}

/// The source basis a member's newest sighting must be at: the gate's own
/// settled measurement.
fn engram_owed_bind_gate(owed: &EngramOwedBind) -> EngramExecutionSourceBasis {
    EngramExecutionSourceBasis {
        workspace_id: owed.measurement.workspace_id.clone(),
        source_revision: owed.measurement.source_revision.clone(),
        source_root_generation: None,
        source_root_state: None,
    }
}

/// One session's owed binds, taken from under the lock for a tick to work
/// off it.
struct EngramOwedBindWork {
    session_id: String,
    target: EngramBindingTarget,
    roots: Vec<EngramWorkSourceRoot>,
    owed: Vec<EngramOwedBind>,
}

/// How a worked entry settled: the line its holder is told, and whether
/// Engram's answer was an identity collision, whose disclosure is also stored
/// in the session's transcript by the commit that retires the entry.
#[derive(Debug, PartialEq, Eq)]
struct EngramOwedBindSettled {
    line: String,
    collision: bool,
}

impl EngramOwedBindSettled {
    fn told(line: String) -> Self {
        Self { line, collision: false }
    }
}

/// What working one session's owed binds left to apply under the lock.
#[derive(Default)]
struct EngramOwedBindResult {
    /// Each worked entry as it now stands, with how it settled when it did.
    entries: Vec<(EngramOwedBind, Option<EngramOwedBindSettled>)>,
    /// Engram refused the session's credentials: rebind before sending.
    refresh: bool,
    /// The credentials the work used (`engram_abort_authority` and routing
    /// token): a rebind is asked for only while they are still the
    /// session's own.
    captured: (String, Option<String>),
}

/// The identity of `target`'s credentials: its authority and routing token.
fn engram_owed_bind_credentials(target: &EngramBindingTarget) -> (String, Option<String>) {
    (engram_abort_authority(target), target.routing_token.clone())
}

impl AppState {
    fn owe_checkpoint_batch_binds_locked(inner: &mut StateInner, session_id: &str,
        operation: &EngramCheckpointOperation, receipt: &EngramCheckpointReceipt) -> Result<bool> {
        let Some(index) = inner.find_session_index(session_id) else { return Ok(false) };
        let record = &inner.sessions[index];
        let selected = record.engram.pending_batches.iter().filter(|batch|
            pending_checkpoint_batch_owner(inner, session_id, batch)
                .is_some_and(|owner| owner.original.id == operation.original.id))
            .cloned().collect::<Vec<_>>();
        if selected.is_empty() { return Ok(false) }
        let EngramControlRequest::TurnCheckpoint { report, grant_id, .. } = &operation.original.request
            else { bail!("Not a checkpoint") };
        if &receipt.grant_id != grant_id || receipt._confirmed_cursor < receipt._cursor
            || receipt.checkpoint.as_ref().is_none_or(|id| !is_engram_record_id(id)) {
            bail!("Checkpoint batch receipt is not confirmed")
        }
        let mut pending = selected.clone();
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let settlement = engram_owe_batch_binds(&mut pending, report, receipt, &now);
        if !pending.is_empty() || !settlement.lines.is_empty() {
            bail!("Checkpoint receipt does not name the exact pending batch inputs")
        }
        let mut owed_binds = record.engram.owed_binds.clone();
        for owed in settlement.owed {
            if let Some(previous) = owed_binds.iter().find(|previous|
                previous.original == owed.original && previous.target.work_id == owed.target.work_id
                    && previous.target.claim_id == owed.target.claim_id
                    && previous.target.claim_fence == owed.target.claim_fence
                    && previous.target.root_generation == owed.target.root_generation) {
                if previous.target != owed.target || previous.measurement != owed.measurement {
                    bail!("Checkpoint batch conflicts with independently owed custody")
                }
            } else { owed_binds.push(owed); }
        }
        let record = inner.session_mut_by_index(index).expect("checkpoint batch owner");
        record.engram.pending_batches.retain(|batch| !selected.contains(batch));
        record.engram.owed_binds = owed_binds;
        Ok(true)
    }

    /// After the checkpoint of `session_id` answered with `receipt` for
    /// `report`: the pending batches of that checkpoint become owed binds,
    /// persisted before any of them is sent, and members that can never be
    /// bound get their lines. Called for an ordinary closing checkpoint and
    /// for the one a rebind's recovery sends alike.
    fn owe_engram_batch_binds(
        &self,
        session_id: &str,
        report: &EngramTurnReport,
        receipt: &EngramCheckpointReceipt,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if Self::owe_engram_batch_binds_locked(&mut inner, session_id, report, receipt)
            && let Err(error) = self.commit_locked(&mut inner) {
            eprintln!("engram> session={session_id} failed persisting owed batch binds: {error:#}");
        }
    }

    fn owe_engram_batch_binds_locked(
        inner: &mut StateInner,
        session_id: &str,
        report: &EngramTurnReport,
        receipt: &EngramCheckpointReceipt,
    ) -> bool {
        let Some(index) = inner.find_session_index(session_id) else {
            return false;
        };
        if inner.sessions[index].engram.pending_batches.is_empty() {
            return false;
        }
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let settlement =
            engram_owe_batch_binds(&mut record.engram.pending_batches, report, receipt, &now);
        if settlement.owed.is_empty() && settlement.lines.is_empty() {
            return false;
        }
        for line in settlement.lines {
            eprintln!("engram> session={session_id} {line}");
            record.engram.set_pending_source_root_line(line);
        }
        record.engram.owed_binds.extend(settlement.owed);
        // Taken pending batches are stored settled, so a restart never tells
        // their members again.
        true
    }

    /// Engram refused the checkpoint of `session_id`'s grant `grant_id`: a
    /// batch gate it credited can no longer be bound, so each member's
    /// holder is told now.
    fn drain_engram_refused_batches(
        inner: &mut StateInner,
        index: usize,
        session_id: &str,
        grant_id: &str,
        code: Option<&str>,
    ) -> bool {
        let lines = Self::take_engram_refused_batches(inner, index, session_id, grant_id, code);
        let drained = !lines.is_empty();
        for line in lines {
            eprintln!("engram> session={session_id} {line}");
            inner.sessions[index].engram.set_pending_source_root_line(line);
        }
        drained
    }

    fn take_engram_refused_batches(
        inner: &mut StateInner,
        index: usize,
        session_id: &str,
        grant_id: &str,
        code: Option<&str>,
    ) -> Vec<String> {
        if !inner.sessions[index]
            .engram
            .pending_batches
            .iter()
            .any(|batch| batch.checkpoint_grant_id == grant_id)
        {
            return Vec::new();
        }
        let protected = inner.sessions[index].engram.pending_batches.iter()
            .filter(|batch| pending_checkpoint_batch_owner(inner, session_id, batch).is_some())
            .cloned().collect::<Vec<_>>();
        // Through the stamping accessor, so the caller's commit stores it.
        let Some(record) = inner.session_mut_by_index(index) else {
            return Vec::new();
        };
        engram_drain_pending_batches(
            &mut record.engram.pending_batches,
            |batch| batch.checkpoint_grant_id == grant_id && !protected.contains(batch),
            &format!(
                "Engram refused the checkpoint that carried the batch gate ({})",
                code.unwrap_or("unknown")
            ),
        )
    }

    /// One tick of the owed binds of every session, on their own thread
    /// (`spawn_engram_retry_tick`), never on a turn's path or the admission
    /// retries' thread. A batch whose checkpoint was not confirmed in time is
    /// drained with its lines; each owed bind due is decided once, its
    /// decision confirmed stored, then sent and settled by Engram's answer,
    /// or given up past its cap. Engram calls stop once `budget` is spent;
    /// what is left waits for the next tick.
    fn engram_owed_binds_tick(&self, now: chrono::DateTime<chrono::Utc>, budget: Duration) {
        if self.engram_retry_slots.boot_preparation_pending() {
            return;
        }
        let deadline = std::time::Instant::now() + budget;
        let work: Vec<EngramOwedBindWork> = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let mut drained = false;
            for index in 0..inner.sessions.len() {
                let session_id = inner.sessions[index].session.id.clone();
                let protected = inner.sessions[index].engram.pending_batches.iter()
                    .filter(|batch| pending_checkpoint_batch_owner(&inner, &session_id, batch).is_some())
                    .cloned().collect::<Vec<_>>();
                let engram = &inner.sessions[index].engram;
                let due = engram
                    .pending_batches
                    .iter()
                    .any(|batch| !protected.contains(batch) && engram_pending_batch_expired(batch, now))
                    || engram.owed_binds.iter().any(|owed| {
                        matches!(engram_owed_bind_step(owed, now), EngramOwedBindStep::GiveUp(_))
                    });
                if !due {
                    continue;
                }
                // Through the stamping accessor, so the commit below stores
                // what settles here.
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                let lines = engram_drain_pending_batches(
                    &mut record.engram.pending_batches,
                    |batch| !protected.contains(batch) && engram_pending_batch_expired(batch, now),
                    &format!(
                        "the checkpoint that carried the batch gate was not confirmed within \
                         {ENGRAM_OWED_BIND_MAX_MINUTES} minutes"
                    ),
                );
                // Giving up needs no Engram call: settled here, so a session
                // with no binding target still tells its holder.
                let (given_up, kept): (Vec<_>, Vec<_>) =
                    std::mem::take(&mut record.engram.owed_binds)
                        .into_iter()
                        .partition(|owed| {
                            matches!(engram_owed_bind_step(owed, now), EngramOwedBindStep::GiveUp(_))
                        });
                record.engram.owed_binds = kept;
                let given_up = given_up.iter().filter_map(|owed| match engram_owed_bind_step(owed, now) {
                    EngramOwedBindStep::GiveUp(line) => Some(line),
                    _ => None,
                });
                for line in lines.into_iter().chain(given_up) {
                    eprintln!("engram> session={} {line}", record.session.id);
                    record.engram.set_pending_source_root_line(line);
                    drained = true;
                }
            }
            if drained && let Err(error) = self.commit_locked(&mut inner) {
                eprintln!("engram> failed persisting settled batch binds: {error:#}");
            }
            inner
                .sessions
                .iter()
                .filter(|record| {
                    record.engram.owed_binds.iter().any(|owed| {
                        engram_owed_bind_step(owed, now) != EngramOwedBindStep::Wait
                    })
                })
                .filter(|record| {
                    // Boot recovery, or a bind, is restoring the session's
                    // credentials: wait for it rather than race it.
                    !record.engram_boot_recovery_pending && !record.engram.bind_in_progress
                })
                .filter(|record| {
                    // A settings change in flight: wait for its authority.
                    !record.engram.project_reset_in_progress
                        && !engram_project_for_session_locked(&inner, &record.session.id)
                            .is_some_and(|project| inner.engram_project_resets.contains(&project.id))
                })
                .filter_map(|record| {
                    let target = Self::engram_binding_target_for_session_shape_locked(
                        &inner,
                        &record.session.id,
                        true,
                    )
                    .ok()
                    .flatten()?;
                    Some(EngramOwedBindWork {
                        session_id: record.session.id.clone(),
                        target,
                        roots: inner.engram_work_source_roots.clone(),
                        owed: record.engram.owed_binds.clone(),
                    })
                })
                .collect()
        };
        for session in work {
            let session_id = session.session_id.clone();
            let result = self.work_engram_owed_binds(session, now, deadline);
            let unconfirmed = self.apply_engram_owed_binds(&session_id, result.entries);
            for owed in unconfirmed {
                self.confirm_engram_owed_bind_decision(&session_id, &owed, deadline);
            }
            // The capped, fenced rebind every lost runtime gets, only while
            // the refused credentials are still the session's own and still
            // marked stale, and only while the session is idle: a session
            // already rebound is left alone, and a busy one is left to its
            // turn. The next tick sends under the token it brings.
            if result.refresh {
                self.rebind_engram_session_for_owed_binds(&session_id, &result.captured);
            }
        }
    }

    /// Rebinds `session_id` for its owed binds' credential refusal, only when
    /// its credentials are still the refused ones `captured`, still marked
    /// stale, and no turn of it is open or being admitted. That rebind reads
    /// the session's status and closes any grant still open with a recovery
    /// checkpoint, or drops it when Engram refuses the token, so a running
    /// turn's grant would lose that turn's execution and check report.
    ///
    /// The check, the target and the claim of the bind (`bind_in_progress`)
    /// are taken in one critical section, so no admission can come between
    /// them: one that came first fails the check, and one that comes after
    /// finds the bind claimed and backs off (the bind path refuses a second
    /// bind of a session). While the session is busy the bind keeps waiting
    /// for fresh credentials: the turn's own admission or checkpoint rebinds
    /// a session marked stale, and a later tick finds it idle otherwise. A
    /// retry head is rebound only under the retry cap, as every runtime-loss
    /// rebind is (`EngramRetryHeadPermit`).
    fn rebind_engram_session_for_owed_binds(
        &self,
        session_id: &str,
        captured: &(String, Option<String>),
    ) {
        let (target, _permit) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            let record = &inner.sessions[index];
            let idle = matches!(
                record.session.status,
                SessionStatus::Idle | SessionStatus::Error
            ) && !record.runtime_stop_in_progress
                && record.engram.admission_in_progress.is_none()
                && record.engram.pending_dispatch.is_none()
                && record.engram.active_grant_id.is_none()
                && record.engram.uncertain_grant_id.is_none()
                && !record.engram.bind_in_progress
                && !record.engram.project_reset_in_progress
                // Boot recovery restores the credentials itself; a rebind
                // here could take the bind from a lazy recovery between its
                // retry claim and its bind, and leave the session fenced.
                && !record.engram_boot_recovery_pending
                && !record.engram_boot_recovery_retry_in_progress;
            if !idle {
                return;
            }
            let Some(target) =
                Self::engram_binding_target_for_session_shape_locked(&inner, session_id, true)
                    .ok()
                    .flatten()
                    .filter(|target| {
                        target.rebind_required && engram_owed_bind_credentials(target) == *captured
                    })
            else {
                return;
            };
            if engram_project_for_session_locked(&inner, session_id)
                .is_some_and(|project| inner.engram_project_resets.contains(&project.id))
            {
                return;
            }
            let permit = match self.engram_retry_head_permit_locked(&inner, session_id) {
                EngramRetryHeadPermit::Deferred => return,
                permit => permit,
            };
            inner
                .session_mut_by_index(index)
                .expect("session index should be valid")
                .engram
                .bind_in_progress = true;
            (target, permit)
        };
        let result = self.bind_engram_target_uncoordinated_off_lock(target, false, None);
        self.clear_engram_bind_in_progress(session_id);
        if let Err(error) = result {
            self.record_engram_transport_failure(session_id, &error);
            eprintln!("engram> session={session_id} owed-bind rebind degraded: {error}");
        }
    }

    /// Records Engram's credential refusal `error` for `session_id`, and asks
    /// for a rebind, only when the refused credentials `captured` are still
    /// the session's own. Returns whether it did.
    fn note_owed_bind_credential_refusal(
        &self,
        session_id: &str,
        captured: &(String, Option<String>),
        error: &EngramTransportError,
    ) -> bool {
        if !engram_status_error_requires_fresh_bind(error) {
            return false;
        }
        // Checked and applied in one critical section, so no successor
        // token can be installed between them.
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let current = Self::engram_binding_target_for_session_shape_locked(&inner, session_id, false)
            .ok()
            .flatten();
        if !current.is_some_and(|current| engram_owed_bind_credentials(&current) == *captured) {
            return false;
        }
        Self::record_engram_transport_failure_locked(&mut inner, session_id, error);
        true
    }

    /// Works one session's owed binds off the lock until `deadline`.
    fn work_engram_owed_binds(
        &self,
        work: EngramOwedBindWork,
        now: chrono::DateTime<chrono::Utc>,
        deadline: std::time::Instant,
    ) -> EngramOwedBindResult {
        let EngramOwedBindWork {
            session_id,
            target,
            roots,
            owed,
        } = work;
        let mut result = EngramOwedBindResult {
            captured: engram_owed_bind_credentials(&target),
            ..EngramOwedBindResult::default()
        };
        // Each Engram call gets what is left of the tick, recomputed before
        // it; none starts once the tick's budget is spent.
        let call_timeout = || {
            deadline
                .checked_duration_since(std::time::Instant::now())
                .filter(|left| !left.is_zero())
                .map(|left| target.settings.call_timeout().min(left))
        };
        let mut held = None;
        for mut entry in owed {
            let step = engram_owed_bind_step(&entry, now);
            if let EngramOwedBindStep::GiveUp(line) = step {
                result.entries.push((entry, Some(EngramOwedBindSettled::told(line))));
                continue;
            }
            if step == EngramOwedBindStep::Wait {
                continue;
            }
            // A decision not confirmed stored yet goes back, unchanged, to be
            // confirmed again; it is neither sent nor decided anew.
            if step == EngramOwedBindStep::Confirm {
                result.entries.push((entry, None));
                continue;
            }
            let Some(timeout) = call_timeout() else {
                continue;
            };
            // Credentials Engram refused, before this tick or in it: the
            // rebind is asked for again every wait, until a fresh token comes
            // or the bind's time cap gives it up. Nothing is sent with the
            // stale token, and the wait counts no attempt.
            if target.rebind_required || result.refresh {
                result.refresh = true;
                engram_owed_bind_wait_for_credentials(&mut entry, now);
                result.entries.push((entry, None));
                continue;
            }
            if step == EngramOwedBindStep::Decide {
                let held = held.get_or_insert_with(|| {
                    target.adapter.read_held_claims(&target.connection, timeout)
                });
                let held = match held {
                    Ok(held) => &*held,
                    Err(error) => {
                        if self.note_owed_bind_credential_refusal(&session_id, &result.captured, error) {
                            result.refresh = true;
                        }
                        let detail = format!("its claim could not be read: {error}");
                        engram_owed_bind_attempt_failed(&mut entry, detail, now);
                        result.entries.push((entry, None));
                        continue;
                    }
                };
                let gate = engram_owed_bind_gate(&entry);
                let run_id = held
                    .items
                    .iter()
                    .find(|claim| {
                        claim.work_id == entry.target.work_id
                            && claim.claim_id == entry.target.claim_id
                    })
                    .and_then(|claim| claim.control_binding.as_ref())
                    .map(|binding| binding.run_id.clone());
                // Without a held, bound claim the decision refuses before it
                // reads a sighting, and says why.
                let decision = match run_id {
                    None => engram_bind_decision(&entry.target, held, &roots, Err("not read"), &gate),
                    Some(run_id) => {
                        // The held read may have spent the tick's budget.
                        let Some(timeout) = call_timeout() else {
                            continue;
                        };
                        match self.read_owed_bind_sighting(
                        &target,
                        &entry.target.work_id,
                        &run_id,
                        timeout,
                    ) {
                        Ok(read) => engram_bind_decision(&entry.target, held, &roots, Ok(&read), &gate),
                        Err(why) => {
                            if self.note_owed_bind_credential_refusal(&session_id, &result.captured, &why) {
                                result.refresh = true;
                            }
                            let detail = format!("its newest sighting could not be read: {why}");
                            engram_owed_bind_attempt_failed(&mut entry, detail, now);
                            result.entries.push((entry, None));
                            continue;
                        }
                    }
                    }
                };
                match decision {
                    // Decided, and sent only once the decision is stored.
                    EngramBindDecision::Bind { binding, sighting } => {
                        entry.decided = Some(EngramOwedBindDecided { binding, sighting });
                        entry.decision_unconfirmed = true;
                        result.entries.push((entry, None));
                    }
                    EngramBindDecision::Skip(reason) => {
                        let line = engram_batch_not_recorded_line(&entry.target.short_ref, &reason);
                        result.entries.push((entry, Some(EngramOwedBindSettled::told(line))));
                    }
                    // Not final: read again on a later attempt, within the cap.
                    EngramBindDecision::Unknown(why) => {
                        engram_owed_bind_attempt_failed(&mut entry, why, now);
                        result.entries.push((entry, None));
                    }
                }
                continue;
            }
            let Some(request) = target
                .routing_token
                .as_deref()
                .and_then(|token| engram_owed_bind_request(&entry, token))
            else {
                let detail = "the session has no routing token yet".to_owned();
                engram_owed_bind_attempt_failed(&mut entry, detail, now);
                result.entries.push((entry, None));
                continue;
            };
            // The send is a control operation on the session's binding: it is
            // claimed with the checks that it may still go, so a settings
            // reset waits for it (or a reset that came first stops it), and a
            // rebind cannot swap the token under it.
            if !self.claim_engram_owed_bind_send(&session_id, &entry.key(), &result.captured) {
                continue;
            }
            let answer = target.adapter.request(&target.connection, &request, timeout);
            self.release_engram_owed_bind_send(&session_id);
            // The session's own credential bookkeeping, as for any call, but
            // only for credentials that are still the session's own.
            if let Err(error) = &answer
                && self.note_owed_bind_credential_refusal(&session_id, &result.captured, error)
            {
                result.refresh = true;
            }
            let outcome = engram_bind_outcome(
                answer,
                &entry.original,
                &entry.target.short_ref,
                &entry.target.work_id,
            );
            let collision = matches!(outcome, EngramBindOutcome::Collision(_));
            let settled = engram_owed_bind_answered(&mut entry, outcome, now)
                .map(|line| EngramOwedBindSettled { line, collision });
            result.entries.push((entry, settled));
        }
        result
    }

    /// Claims `session_id`'s binding (`bind_in_progress`) for sending the
    /// owed bind `key`, in one critical section with the checks that it may
    /// still go: the entry is still owed (no settings reset dropped it), no
    /// settings reset is pending or running for the session's project, no
    /// boot recovery or other bind holds the binding, and the session's
    /// credentials are still `captured` and not marked stale. A settings
    /// reset waits for a claimed binding to settle before it drops the owed
    /// binds (`wait_for_engram_project_reset_quiescence`), so a send either
    /// finishes before the reset settles or never starts. Returns whether it
    /// claimed; an entry not claimed is left as it is stored, for a later
    /// tick.
    fn claim_engram_owed_bind_send(
        &self,
        session_id: &str,
        key: &(String, String),
        captured: &(String, Option<String>),
    ) -> bool {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return false;
        };
        if engram_project_for_session_locked(&inner, session_id)
            .is_some_and(|project| inner.engram_project_resets.contains(&project.id))
        {
            return false;
        }
        let record = &inner.sessions[index];
        if record.engram.bind_in_progress
            || record.engram.project_reset_in_progress
            || record.engram_boot_recovery_pending
            || !record.engram.owed_binds.iter().any(|owed| owed.key() == *key)
        {
            return false;
        }
        let current_credentials =
            Self::engram_binding_target_for_session_shape_locked(&inner, session_id, true)
                .ok()
                .flatten()
                .filter(|current| !current.rebind_required)
                .map(|current| engram_owed_bind_credentials(&current));
        if current_credentials.as_ref() != Some(captured) {
            return false;
        }
        inner
            .session_mut_by_index(index)
            .expect("session index should be valid")
            .engram
            .bind_in_progress = true;
        true
    }

    /// Releases the binding `claim_engram_owed_bind_send` claimed.
    fn release_engram_owed_bind_send(&self, session_id: &str) {
        self.clear_engram_bind_in_progress(session_id);
    }

    /// Applies one session's worked `entries` to its own list under the lock:
    /// a settled entry leaves with its line, the others are replaced as they
    /// now stand. An entry no longer there (a settings reset dropped it and
    /// told its holder) is left alone, its line unsaid. Returns the entries
    /// whose decision is to be confirmed stored.
    fn apply_engram_owed_binds(
        &self,
        session_id: &str,
        entries: Vec<(EngramOwedBind, Option<EngramOwedBindSettled>)>,
    ) -> Vec<EngramOwedBind> {
        if entries.is_empty() {
            return Vec::new();
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return Vec::new();
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let mut unconfirmed = Vec::new();
        let mut changed = false;
        let mut collisions = Vec::new();
        for (entry, settled) in entries {
            let Some(position) = record
                .engram
                .owed_binds
                .iter()
                .position(|current| current.key() == entry.key())
            else {
                continue;
            };
            match settled {
                Some(EngramOwedBindSettled { line, collision }) => {
                    record.engram.owed_binds.remove(position);
                    eprintln!("engram> session={session_id} {line}");
                    // An identity collision retires its entry only with a
                    // durable disclosure: the transcript notice below is
                    // stored by the same commit as this removal. The pending
                    // line, which is not stored, also reaches the next prompt.
                    if collision {
                        collisions.push(line.clone());
                    }
                    record.engram.set_pending_source_root_line(line);
                    changed = true;
                }
                None => {
                    if entry.decision_unconfirmed {
                        unconfirmed.push(entry.clone());
                    }
                    // A decision only waiting for its store confirmation
                    // comes back unchanged: nothing to store.
                    if record.engram.owed_binds[position] != entry {
                        record.engram.owed_binds[position] = entry;
                        changed = true;
                    }
                }
            }
        }
        for text in collisions {
            let message = Message::Text {
                attachments: Vec::new(),
                id: inner.next_message_id(),
                timestamp: stamp_now(),
                author: Author::System,
                text,
                expanded_text: None,
                source: None,
            };
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            push_message_on_record(record, message);
        }
        if changed && let Err(error) = self.commit_locked(&mut inner) {
            eprintln!("engram> session={session_id} failed persisting owed batch binds: {error:#}");
        }
        unconfirmed
    }

    /// Confirms `owed`'s decision stored, by reading it back from the durable
    /// session row (`PersistFenceTarget::EngramOwedBindDecision`), within
    /// what is left before `deadline`; then lets it be sent. Unconfirmed, it
    /// stays unsent and a later tick confirms it again.
    fn confirm_engram_owed_bind_decision(
        &self,
        session_id: &str,
        owed: &EngramOwedBind,
        deadline: std::time::Instant,
    ) {
        let Ok(decided) = serde_json::to_value(&owed.decided) else {
            return;
        };
        let timeout = deadline
            .checked_duration_since(std::time::Instant::now())
            .unwrap_or_default()
            .max(ENGRAM_OWED_BIND_CONFIRM_FLOOR);
        let target = PersistFenceTarget::EngramOwedBindDecision {
            session_id: session_id.to_owned(),
            original: owed.original.clone(),
            work_id: owed.target.work_id.clone(),
            decided,
        };
        if let Err(error) = self.confirm_persist_fence_target(target, timeout) {
            eprintln!(
                "engram> session={session_id} the batch bind decision for {} is not confirmed \
                 stored yet: {error}",
                owed.target.short_ref
            );
            return;
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        if let Some(current) = inner.sessions[index]
            .engram
            .owed_binds
            .iter_mut()
            .find(|current| current.key() == owed.key() && current.decided == owed.decided)
        {
            current.decision_unconfirmed = false;
        }
    }

    /// Waits within `timeout` for `target` to be read back durable, as an
    /// owed close is (`confirm_engram_owed_close_durable`).
    fn confirm_persist_fence_target(
        &self,
        target: PersistFenceTarget,
        timeout: Duration,
    ) -> std::result::Result<(), String> {
        let clock = self.engram_budget_clock();
        let deadline = clock.now() + timeout;
        let (fence, waiter) = PersistFence::new_with_clock(target.clone(), deadline, clock.clone());
        if self.persist_tx.send(PersistRequest::Fence(Box::new(fence))).is_ok() {
            return waiter.wait().map_err(|error| format!("{error:?}"));
        }
        if !engram_authority_manual_writer_allowed() {
            return Err("the persistence writer stopped".to_owned());
        }
        #[cfg(test)]
        {
            let delta = collect_persist_delta_from_shared_state(&self.inner, 0);
            let mut cache = SqlitePersistConnectionCache::new();
            persist_delta_via_cache(&mut cache, self.persistence_path.as_path(), &delta)
                .map_err(|error| format!("{error:#}"))?;
            if !cache
                .connection
                .as_ref()
                .is_some_and(|connection| target.is_already_durable(connection).unwrap_or(false))
            {
                return Err("the content was not committed".to_owned());
            }
        }
        Ok(())
    }

    /// The newest sighting of `work_id`'s run `run_id`, read at the head of
    /// that run through the session's connection.
    fn read_owed_bind_sighting(
        &self,
        target: &EngramBindingTarget,
        work_id: &str,
        run_id: &str,
        timeout: Duration,
    ) -> std::result::Result<EngramNamedRootSightingRead, EngramTransportError> {
        let request = EngramControlRequest::NamedRootSightingRead {
            work_ref: work_id.to_owned(),
            run_id: run_id.to_owned(),
            run_cut: None,
        };
        let value = target
            .adapter
            .request(&target.connection, &request, timeout)?;
        if serde_json::to_vec(&value)
            .map_or(true, |bytes| bytes.len() > ENGRAM_NAMED_ROOT_SIGHTING_READ_MAX_BYTES)
        {
            return Err(EngramTransportError::protocol("the answer exceeds 16 KiB"));
        }
        let read = parse_engram_named_root_sighting_read(value).map_err(EngramTransportError::protocol)?;
        if read.schema_version != ENGRAM_NAMED_ROOT_SIGHTING_READ_SCHEMA_VERSION {
            return Err(EngramTransportError::protocol(format!(
                "the answer has schema version {}, not {ENGRAM_NAMED_ROOT_SIGHTING_READ_SCHEMA_VERSION}",
                read.schema_version
            )));
        }
        Ok(read)
    }
}
