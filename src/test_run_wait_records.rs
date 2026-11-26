// A run wait's view of its runs' Engram verification records.
//
// Owns: the record text a run wait's resume prompt carries, marking a run
// whose verdict resumed the wait while its record was still open, and the
// later resumes that name each such record once it is settled, refused, or
// left pending by the host's latest look.
//
// Does not own: sending or settling records (engram_verification_record.rs),
// the settled-record line a session's next prompt carries
// (engram_received_check_notice.rs), the first resume's verdicts and
// budgets (test_run_waits.rs), or the queue and dispatch of resumes.
//
// New file, a child module of test_run_waits.rs beside
// test_run_wait_observations.rs.

use super::*;

/// Most record ids a prompt line names for one run.
const TEST_RUN_WAIT_RECORD_IDS_MAX: usize = 8;
/// Most bytes of a refusal's detail a prompt line carries.
const TEST_RUN_WAIT_RECORD_DETAIL_MAX_BYTES: usize = 512;
/// Most bytes of one record id a prompt line carries.
const TEST_RUN_WAIT_RECORD_ID_MAX_BYTES: usize = 64;
/// Said of a run whose record is still open at the first resume.
const TEST_RUN_WAIT_RECORD_FOLLOW_UP: &str = "; this wait resumes once more when Engram \
    settles or refuses it, or the host leaves it unsent";

/// Whether a run's record reached its last state: settled, or refused by
/// Engram or by this host. A received receipt is not settled yet.
fn test_run_wait_record_final(ack: Option<&TestRunVerificationAck>) -> bool {
    matches!(
        ack,
        Some(
            TestRunVerificationAck::Settled { .. }
                | TestRunVerificationAck::Rejected { .. }
                | TestRunVerificationAck::LocalUnclassified { .. }
        )
    )
}

/// The bound registration of the run a wait's label admitted: the one under
/// its run id whose request and locator are those the label was admitted
/// with. A registration under a reused run id, for another request, is not
/// this run's: its verdict and record are never this wait's.
pub(super) fn test_run_wait_label_registration_locked<'a>(
    inner: &'a StateInner,
    label: &TestRunWaitRunLabel,
) -> Option<&'a TestRunRegistration> {
    inner
        .test_run_registrations
        .get(&label.run_id)
        .filter(|row| {
            engram_verification_record_registration_bound(row)
                && label.request_identity.as_ref() == Some(&row.request_identity)
                && label.locator_identity.is_some()
                && label.locator_identity == row.locator_identity
        })
}

/// Whether a wait's run has a bound registration of its own whose record is
/// final, so that its wait verdict can come from that record
/// (`test_run_wait_record_verdict_locked`).
pub(super) fn test_run_wait_record_final_locked(
    inner: &StateInner,
    label: &TestRunWaitRunLabel,
) -> bool {
    test_run_wait_label_registration_locked(inner, label)
        .is_some_and(|row| test_run_wait_record_final(row.verification_record.as_ref()))
}

/// Whether a record line names a record in its last state, settled or
/// refused (`test_run_wait_record_state`): such a run has finished, and
/// needs no recovery.
pub(super) fn test_run_wait_record_text_final(text: &str) -> bool {
    text.starts_with("settled;") || text.starts_with("refused")
}

/// A run's wait verdict from the host's own record, for a run the index has
/// no verdict for: one it reads as unknown, one whose latest read failed, or
/// one it has not listed (`summary` `None`) or still reads as running through
/// a full rescan begun after the record became final. A bound run whose record is final
/// has finished, since the producer built and sent that record from the
/// run's terminal results, so the host's record outranks the index's copy.
/// A settled record's run outcome is the verdict; an indeterminate outcome,
/// or a record refused by Engram or by this host, is unknown for the index's
/// own reason, or not indexed. `None` while the record is not final: the
/// index keeps deciding.
pub(super) fn test_run_wait_record_verdict_locked(
    inner: &StateInner,
    label: &TestRunWaitRunLabel,
    summary: Option<&TestRunSummary>,
) -> Option<TestRunWaitVerdict> {
    let registration = test_run_wait_label_registration_locked(inner, label)?;
    let unknown = || {
        TestRunWaitVerdict::Unknown(match summary {
            None => TestRunCardUnknownReason::NotIndexed,
            Some(summary) => summary
                .unknown_reason
                .map_or(TestRunCardUnknownReason::ResultsUnreadable, Into::into),
        })
    };
    match registration.verification_record.as_ref()? {
        TestRunVerificationAck::Settled { receipt, .. } => Some(match receipt.run_outcome {
            EngramVerificationRecordRunOutcome::Passed => TestRunWaitVerdict::Pass,
            EngramVerificationRecordRunOutcome::Failed => TestRunWaitVerdict::Fail,
            EngramVerificationRecordRunOutcome::Indeterminate => unknown(),
        }),
        TestRunVerificationAck::Rejected { .. }
        | TestRunVerificationAck::LocalUnclassified { .. } => Some(unknown()),
        TestRunVerificationAck::Received { .. } => None,
    }
}

fn test_run_wait_receipt_text(receipt: &EngramVerificationRecordReceipt) -> String {
    let outcome = match receipt.run_outcome {
        EngramVerificationRecordRunOutcome::Passed => "passed",
        EngramVerificationRecordRunOutcome::Failed => "failed",
        EngramVerificationRecordRunOutcome::Indeterminate => "indeterminate",
    };
    let mut records = receipt
        .verifications
        .iter()
        .take(TEST_RUN_WAIT_RECORD_IDS_MAX)
        .map(|record| {
            let id = record.record_id.replace(['`', '\r', '\n'], "'");
            format!(
                "`{}`",
                test_run_card_cut(&id, TEST_RUN_WAIT_RECORD_ID_MAX_BYTES).0
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let more = receipt
        .verifications
        .len()
        .saturating_sub(TEST_RUN_WAIT_RECORD_IDS_MAX);
    if more > 0 {
        records.push_str(&format!(" (+{more} more in the run record)"));
    }
    if records.is_empty() {
        records = "none named".to_owned();
    }
    format!("Engram record(s) {records}; run outcome {outcome}")
}

fn test_run_wait_refusal_text(detail: &str) -> String {
    let detail = detail.replace(['\r', '\n', '`'], " ");
    let (detail, cut) = test_run_card_cut(&detail, TEST_RUN_WAIT_RECORD_DETAIL_MAX_BYTES);
    format!("{detail}{}", if cut { " (cut)" } else { "" })
}

/// The run's record as the host holds it now, for a prompt line.
fn test_run_wait_record_state(registration: Option<&TestRunRegistration>) -> String {
    match registration.and_then(|row| row.verification_record.as_ref()) {
        Some(TestRunVerificationAck::Settled { receipt, .. }) => {
            format!("settled; {}", test_run_wait_receipt_text(receipt))
        }
        Some(TestRunVerificationAck::Received { receipt, .. }) => format!(
            "received, not yet settled; {}",
            test_run_wait_receipt_text(receipt)
        ),
        Some(TestRunVerificationAck::Rejected { detail, .. }) => {
            format!("refused: {}", test_run_wait_refusal_text(detail))
        }
        Some(TestRunVerificationAck::LocalUnclassified { detail }) => format!(
            "refused by this host before sending: {}",
            test_run_wait_refusal_text(detail)
        ),
        None if registration.is_none() => {
            "unavailable; the run's registration was retired".to_owned()
        }
        None => "not sent yet".to_owned(),
    }
}

/// The longest record text a run's section can carry, for the admission
/// bound on mandatory prompt content.
pub(super) fn test_run_wait_record_mandatory_bound() -> String {
    let id = "x".repeat(TEST_RUN_WAIT_RECORD_ID_MAX_BYTES);
    let receipt = EngramVerificationRecordReceipt {
        operation: String::new(),
        producer: String::new(),
        verifications: vec![
            EngramVerificationRecordVerification {
                stage: String::new(),
                record_id: id,
            };
            TEST_RUN_WAIT_RECORD_IDS_MAX + 1
        ],
        run_outcome: EngramVerificationRecordRunOutcome::Indeterminate,
    };
    // The "+N more" count may need up to twenty digits, not one.
    let received = format!(
        "received, not yet settled; {}{TEST_RUN_WAIT_RECORD_FOLLOW_UP}{}",
        test_run_wait_receipt_text(&receipt),
        " ".repeat(19)
    );
    let refused = format!(
        "refused by this host before sending: {}",
        test_run_wait_refusal_text(&"x".repeat(TEST_RUN_WAIT_RECORD_DETAIL_MAX_BYTES + 1))
    );
    // A first resume can also name a reason a look left the record pending
    // for good, with the records Engram already gave for it.
    let pending = [
        EngramVerificationRecordPending::AcknowledgedBodyChanged,
        EngramVerificationRecordPending::InitiatingSessionGone,
        EngramVerificationRecordPending::RootGenerationOutOfRange { generation: 0 },
        EngramVerificationRecordPending::StartedBeforeRegistration { stage: None },
    ]
    .iter()
    .map(|reason| {
        format!(
            "{}{}",
            test_run_wait_pending_text(reason, Some(&receipt)),
            " ".repeat(19)
        )
    })
    .max_by_key(String::len)
    .unwrap_or_default();
    [received, refused, pending]
        .into_iter()
        .max_by_key(String::len)
        .unwrap_or_default()
}

/// What the first resume does with a run whose record is open but whose
/// own results do not decide it: a verdict unknown, or a pass or failure
/// whose results could not be read.
enum TestRunWaitOpenRecord {
    /// A producer look is under way: the resume waits for it.
    Await(u64),
    /// The host's latest look left the record pending for good, so named.
    Named(String),
    /// A record of it may be in flight: followed.
    Follow,
    /// Nothing of it to follow.
    Unfollowed,
}

fn test_run_wait_open_record_locked(
    inner: &StateInner,
    label: &TestRunWaitRunLabel,
    registration: &TestRunRegistration,
) -> TestRunWaitOpenRecord {
    if let Some(awaited) = test_run_wait_look_to_await(
        inner,
        &label.run_id,
        label.registration_progress.record_deferred_for,
        label.registration_progress.record_deferred,
    ) {
        return TestRunWaitOpenRecord::Await(awaited);
    }
    if let Some(EngramVerificationRecordLook::Pending(reason)) =
        inner.test_run_record_pending.get(&label.run_id)
        && test_run_wait_pending_ends_wait(reason)
    {
        let received = match registration.verification_record.as_ref() {
            Some(TestRunVerificationAck::Received { receipt, .. }) => Some(receipt),
            _ => None,
        };
        return TestRunWaitOpenRecord::Named(test_run_wait_pending_text(reason, received));
    }
    if test_run_wait_record_may_be_in_flight(inner, registration) {
        return TestRunWaitOpenRecord::Follow;
    }
    TestRunWaitOpenRecord::Unfollowed
}

/// The bound runs that passed or failed in waits a refresh may resume, each
/// with its run directory, for the off-lock read of whether its results
/// carry a committed-input record.
pub(super) fn test_run_wait_input_reads_locked(
    inner: &StateInner,
    summaries: &[Option<&TestRunSummary>],
) -> Vec<(String, PathBuf)> {
    summaries
        .iter()
        .flatten()
        .filter(|summary| matches!(summary.state, TestRunState::Passed | TestRunState::Failed))
        .filter(|summary| {
            inner
                .test_run_registrations
                .get(&summary.run_id)
                .is_some_and(|registration| {
                    registration.external_launcher.is_none()
                        && engram_verification_record_registration_bound(registration)
                })
        })
        .filter_map(|summary| {
            let entry = inner.test_runs.find(&summary.run_id)?;
            Some((summary.run_id.clone(), entry.disk.run_dir.clone()))
        })
        .collect()
}

/// Whether a run's results carry a committed-input record, which a
/// launcher run's verification record is built from. Read off the state lock.
/// `None` when the results cannot be read now (missing, partly written or
/// unreadable): never taken for a run that has no record to follow up. The
/// first resume then follows the run, waits for one producer look under way,
/// or names a reason that look left its record pending for good; only a run
/// whose results were not read at all waits for a later refresh.
pub(super) fn test_run_wait_results_have_committed_input(run_dir: &FsPath) -> Option<bool> {
    let TestRunJson::Parsed(results, _, _) = test_run_read_json(&run_dir.join("results.json"))
    else {
        return None;
    };
    Some(TestRunCommittedInput::read(&results).is_some())
}

/// The first resume of `wait`: for each run that passed or failed and sends
/// a record, the text its section names that record by (keyed by run id).
/// A run whose record is still open is marked, so the wait stays to resume
/// once more; the second value says whether any run was marked. A run whose
/// verdict is unknown is marked only when a record of it may be in flight
/// (`test_run_wait_record_may_be_in_flight`), and named with the reason when
/// the host's latest look left its record pending for good; a producer look
/// of it still in flight is waited for first, one look at most
/// (`test_run_wait_look_to_await`). A run whose verdict is not in yet, in a
/// wait another run settled, is never marked: its record is named only when
/// already final, and its later settlement reaches the run owner's next turn
/// alone. A launcher run whose results carry no committed-input record is
/// not marked: no record can be built from it. External launcher terminals
/// still get a post-terminal look for the original launch association.
/// `inputs` answers that question for the runs read off the lock; `None`
/// asks the caller to wait for the next refresh.
pub(super) fn test_run_wait_first_resume_records_locked(
    inner: &StateInner,
    wait: &mut TestRunWaitRecord,
    verdicts: &[Option<TestRunWaitVerdict>],
    inputs: &HashMap<String, Option<bool>>,
) -> Option<(HashMap<String, String>, bool)> {
    let mut records = HashMap::new();
    let mut marks = Vec::new();
    let mut deferral = None;
    for (index, (label, verdict)) in wait.runs.iter().zip(verdicts).enumerate() {
        if label.remote.is_some() || label.observation.is_some() {
            continue;
        }
        // Only a run bound through its named root sends a record, and only
        // the registration this label admitted is this run's.
        let Some(registration) = test_run_wait_label_registration_locked(inner, label) else {
            continue;
        };
        if !matches!(
            verdict,
            Some(TestRunWaitVerdict::Pass | TestRunWaitVerdict::Fail)
        ) {
            // A record already final is named even while the index has not
            // caught up with the run's verdict. An open one is followed only
            // when the run's verdict is unknown (its directory removed, say)
            // and a record of it may be in flight. A look still in flight
            // decides that first: the next refresh reads what it came to. A
            // run whose verdict is not in yet (in a wait another run settled)
            // is not followed: it has not finished as far as the index shows.
            // Nor is one reported unknown because its registered files could
            // not be read: its later settlement reaches its owner alone.
            if test_run_wait_record_final(registration.verification_record.as_ref()) {
                records.insert(
                    label.run_id.clone(),
                    test_run_wait_record_state(Some(registration)),
                );
            } else if matches!(
                verdict,
                Some(TestRunWaitVerdict::Unknown(_) | TestRunWaitVerdict::UnknownRegistration(_))
            ) && verdict
                != &Some(TestRunWaitVerdict::UnknownRegistration(
                    TestRunWaitRegistrationUnknownReason::ArtifactsUnreadable,
                ))
            {
                match test_run_wait_open_record_locked(inner, label, registration) {
                    TestRunWaitOpenRecord::Await(awaited) => {
                        deferral = Some((index, awaited));
                        break;
                    }
                    TestRunWaitOpenRecord::Named(text) => {
                        records.insert(label.run_id.clone(), text);
                    }
                    TestRunWaitOpenRecord::Follow => {
                        let mut state = test_run_wait_record_state(Some(registration));
                        state.push_str(TEST_RUN_WAIT_RECORD_FOLLOW_UP);
                        records.insert(label.run_id.clone(), state);
                        marks.push(index);
                    }
                    TestRunWaitOpenRecord::Unfollowed => {}
                }
            }
            continue;
        }
        let mut state = test_run_wait_record_state(Some(registration));
        if !test_run_wait_record_final(registration.verification_record.as_ref()) {
            // A declared native run's record is built from its own input. A
            // run whose results were not read yet waits for the next refresh.
            // One whose results could not be read is never held against the
            // wait's other runs, nor dropped on that failed read: it is
            // followed, unless a look under way is waited for or the host's
            // latest look left its record pending for good.
            let buildable = if registration.declared_native.is_some()
                || registration.external_launcher.is_some()
            {
                true
            } else {
                match *inputs.get(&label.run_id)? {
                    Some(committed) => committed,
                    None => match test_run_wait_open_record_locked(inner, label, registration) {
                        TestRunWaitOpenRecord::Await(awaited) => {
                            deferral = Some((index, awaited));
                            break;
                        }
                        TestRunWaitOpenRecord::Named(text) => {
                            records.insert(label.run_id.clone(), text);
                            continue;
                        }
                        TestRunWaitOpenRecord::Follow | TestRunWaitOpenRecord::Unfollowed => true,
                    },
                }
            };
            if !buildable {
                records.insert(
                    label.run_id.clone(),
                    "none; the run's results carry no committed-input record".to_owned(),
                );
                continue;
            }
            marks.push(index);
            state.push_str(TEST_RUN_WAIT_RECORD_FOLLOW_UP);
        }
        records.insert(label.run_id.clone(), state);
    }
    if let Some((index, awaited)) = deferral {
        let progress = &mut wait.runs[index].registration_progress;
        progress.record_deferred_for = Some(awaited);
        progress.record_deferred = true;
        return None;
    }
    for index in &marks {
        let progress = &mut wait.runs[*index].registration_progress;
        progress.record_follow_up = true;
        // Only a look begun from here on read the run's files after its
        // terminal results were indexed.
        progress.record_follow_up_after = inner.test_run_look_sequence;
    }
    Some((records, !marks.is_empty()))
}

/// Whether a record of a run whose verdict is unknown may still be in flight:
/// Engram received it, this process froze its body (and may be sending it from
/// memory), or the producer's latest look is retrying its send. Such a run is
/// followed like a failed one: it ends by settlement, by a pending reason (the
/// held receipt's body gone with the directory), or by a look begun after the
/// mark that finds no record. A run whose results never made a record, such as
/// one whose launcher died while running, is never followed.
fn test_run_wait_record_may_be_in_flight(
    inner: &StateInner,
    registration: &TestRunRegistration,
) -> bool {
    matches!(
        registration.verification_record,
        Some(TestRunVerificationAck::Received { .. })
    ) || inner
        .test_run_record_bodies_built
        .contains(&registration.run_id)
        || matches!(
            inner.test_run_record_pending.get(&registration.run_id),
            Some(EngramVerificationRecordLook::Pending(reason))
                if !test_run_wait_pending_ends_wait(reason)
        )
        || matches!(
            inner.test_run_record_pending.get(&registration.run_id),
            Some(EngramVerificationRecordLook::ExternalLaunchCustodyMissing { .. })
        )
}

/// How many of the wait's runs it has deferred once for, so that a caller
/// persists each run's one deferral when it is first made.
pub(super) fn test_run_wait_deferred_runs(wait: &TestRunWaitRecord) -> usize {
    wait.runs
        .iter()
        .filter(|label| label.registration_progress.record_deferred)
        .count()
}

/// The look a wait on `run_id` still waits for before deciding, if any: one
/// in flight that has not published yet. A wait defers once per run, for the
/// run's look in flight when it first deferred on it (`deferred_for`): once
/// that look has published, it decides from what the run's latest completed
/// look published, even when a newer look has begun since, and a wait that
/// has deferred once for a run's look (`run_deferred`) never defers again for
/// a later look of that run. So no wait defers longer than one look per run,
/// and none decides from a reason older than the look in flight when it
/// deferred.
fn test_run_wait_look_to_await(
    inner: &StateInner,
    run_id: &str,
    deferred_for: Option<u64>,
    run_deferred: bool,
) -> Option<u64> {
    let in_flight = *inner.test_run_record_looks_in_flight.get(run_id)?;
    let awaited = match deferred_for {
        Some(awaited) => awaited,
        None if run_deferred => return None,
        None => in_flight,
    };
    let published = inner.test_run_record_look_published.get(run_id).copied();
    (!published.is_some_and(|published| published >= awaited)).then_some(awaited)
}

/// Whether a pending reason ends a marked run's wait, since a pending record
/// may stay unsent for good: every reason except those the producer answers
/// by sending the same body again, namely Engram's deferral, a call that
/// failed without an answer, and a held receipt that does not answer the body.
fn test_run_wait_pending_ends_wait(reason: &EngramVerificationRecordPending) -> bool {
    !matches!(
        reason,
        EngramVerificationRecordPending::RemoteDeferred { .. }
            | EngramVerificationRecordPending::TransportFailed { .. }
            | EngramVerificationRecordPending::ReceiptMismatch { .. }
    )
}

/// A marked run's record state for its second resume: `None` while it is
/// still being sent. It is decided from the host's own memory, never from
/// the state file beside the run, which can be older or unwritten:
/// `unassociated` is the body digest of a held receipt whose settlement found
/// no launch observation carrying its records (`test_run_unassociated_receipts`);
/// `look` is what the producer's latest look came to (`test_run_record_pending`),
/// whose pending reason ends the wait unless the producer answers it by
/// sending again (`test_run_wait_pending_ends_wait`), and which also says
/// when the run's own files no longer make a record. That last counts only
/// for a look begun at or after `marked_after`, the look sequence when the
/// wait marked the run: an earlier look may have read a native run's results
/// while the host was still publishing its terminal ones. The send step's
/// reasons and a held receipt's come after the run's terminal results were
/// read, so they are not fenced. The wait reads no run file itself. With no
/// counted look yet (a restarted host before the producer's first look, or
/// no look since the mark) the wait keeps waiting.
fn test_run_wait_follow_up_state(
    registration: Option<&TestRunRegistration>,
    unassociated: Option<&str>,
    look: Option<&EngramVerificationRecordLook>,
    marked_after: u64,
    look_awaited: bool,
) -> Option<String> {
    let ack = registration.and_then(|row| row.verification_record.as_ref());
    if registration.is_none() || test_run_wait_record_final(ack) {
        return Some(test_run_wait_record_state(registration));
    }
    // A look in flight may replace what an earlier one found: waited for,
    // one look at most, before anything else ends the wait.
    if look_awaited {
        return None;
    }
    // Received by Engram, but this host cannot settle it while no launch
    // observation of the run carries its records: named with its records.
    if let Some(TestRunVerificationAck::Received {
        receipt,
        body_digest,
    }) = ack
        && unassociated == Some(body_digest.as_str())
    {
        return Some(format!(
            "received, not yet settled; {}; no launch observation of the run carries it, so \
            this host cannot settle it yet. If it settles later, the run owner's next prompt \
            names it on a \"Verification record settled\" line, and the launcher summary of \
            the run reports it",
            test_run_wait_receipt_text(receipt)
        ));
    }
    if let Some(EngramVerificationRecordLook::Pending(reason)) = look
        && test_run_wait_pending_ends_wait(reason)
    {
        // A record Engram already received is named with its records.
        let received = match ack {
            Some(TestRunVerificationAck::Received { receipt, .. }) => Some(receipt),
            _ => None,
        };
        return Some(test_run_wait_pending_text(reason, received));
    }
    if matches!(look,
        Some(EngramVerificationRecordLook::ExternalLaunchCustodyMissing { begun })
            if *begun >= marked_after)
    {
        return Some(
            "not sent yet (external_launch_custody_missing); the host's latest look found no \
            matching foreground command and RUN locator in complete output. If the original \
            command output arrives later, it can supply custody. Sending still requires an \
            eligible run."
                .to_owned(),
        );
    }
    let no_record = matches!(
        look,
        Some(EngramVerificationRecordLook::NoRecord { begun }) if *begun >= marked_after
    );
    no_record.then(|| match ack {
        // A record Engram already received is named with its records.
        Some(TestRunVerificationAck::Received { receipt, .. }) => format!(
            "received, not yet settled; {}; Engram has it, but the run's request or results \
            no longer make a record this host can settle it from (its run directory was \
            removed or its files rewritten)",
            test_run_wait_receipt_text(receipt)
        ),
        _ => "not sent; the run's request or results no longer make a record (its run \
            directory was removed or its files rewritten)"
            .to_owned(),
    })
}

/// The prompt text of a pending reason that ends a marked run's wait, naming
/// the records of a receipt Engram already gave for it.
fn test_run_wait_pending_text(
    reason: &EngramVerificationRecordPending,
    received: Option<&EngramVerificationRecordReceipt>,
) -> String {
    let kind = serde_json::to_value(reason)
        .ok()
        .and_then(|value| value.get("kind").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned());
    match received {
        Some(receipt) => format!(
            "received, not yet settled; {}; Engram has it, but this host left it pending \
            ({kind}) and cannot settle it. If it settles later, the run owner's next prompt \
            names it on a \"Verification record settled\" line, and the launcher summary of \
            the run reports it",
            test_run_wait_receipt_text(receipt)
        ),
        None => format!(
            "not settled; the host left it pending ({kind}). If Engram records it later, the \
            run owner's next prompt names it on a \"Verification record settled\" line, and \
            the launcher summary of the run reports it"
        ),
    }
}

/// A later resume of a wait whose runs were marked at its first: the prompt
/// naming each marked run whose record settled, was refused or left pending,
/// or whose registration is gone, with those runs' ids. `None` while every
/// marked run's record is still being sent.
pub(super) fn test_run_wait_record_follow_up_locked(
    inner: &StateInner,
    wait: &mut TestRunWaitRecord,
) -> Option<(String, Vec<String>)> {
    let mut lines = Vec::new();
    let mut reported = Vec::new();
    let mut open = 0usize;
    for label in wait
        .runs
        .iter_mut()
        .filter(|label| label.registration_progress.record_follow_up)
    {
        let awaited = test_run_wait_look_to_await(
            inner,
            &label.run_id,
            label.registration_progress.record_deferred_for,
            label.registration_progress.record_deferred,
        );
        if let Some(awaited) = awaited {
            label.registration_progress.record_deferred_for = Some(awaited);
            label.registration_progress.record_deferred = true;
        }
        // A run whose record is still being sent waits on its own: the
        // records that reached an outcome are named now.
        // A registration that is no longer the one this label admitted is
        // gone for this wait.
        let Some(state) = test_run_wait_follow_up_state(
            test_run_wait_label_registration_locked(inner, label),
            inner
                .test_run_unassociated_receipts
                .get(&label.run_id)
                .map(String::as_str),
            inner.test_run_record_pending.get(&label.run_id),
            label.registration_progress.record_follow_up_after,
            awaited.is_some(),
        ) else {
            open += 1;
            continue;
        };
        lines.push(format!("- `{}`: {state}", label.run_id));
        reported.push(label.run_id.clone());
    }
    if reported.is_empty() {
        return None;
    }
    let still_open = if open == 0 {
        String::new()
    } else {
        format!(
            "\n\n{open} more run(s) of this wait still have a record being sent; this wait \
            resumes again when each reaches an outcome."
        )
    };
    let title = wait.title.as_deref().unwrap_or("Test run wait completed");
    let prompt = truncate_to_byte_limit_with_marker(
        format!(
            "{title}: verification records\n\nWait id: `{}`\nSession: `{}`\n\nThis wait \
            resumed earlier on its runs' verdicts. Their Engram verification records:\n{}{still_open}",
            wait.id,
            wait.session_id,
            lines.join("\n")
        ),
        TEST_RUN_WAIT_PROMPT_MAX_BYTES,
        DELEGATION_WAIT_RESUME_TRUNCATED_MARKER,
    );
    Some((prompt, reported))
}
