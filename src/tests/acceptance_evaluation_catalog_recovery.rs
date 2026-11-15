// Real request/read owners with scripted catalog events, never a live store.
use super::*;

const LARGE_TOTAL: u64 = 174;
const PAGE_SIZE: u64 = 8;

fn walk_large_catalog(invalidate: bool) {
    let (state, _, id, child, key) =
        recovery_fixture_with_catalog("independent_session", false, LARGE_TOTAL, PAGE_SIZE);
    let changed = std::cell::Cell::new(false);
    let calls = AtomicUsize::new(0);
    let start = std::time::Instant::now();
    let read = |_: &EngramConnectionConfig, args: &[String], _: Duration| {
        calls.fetch_add(1, Ordering::SeqCst);
        if args.iter().any(|arg| arg == "inspect") {
            return Ok(evidence_selection::canonical_core_receipt());
        }
        let index = args
            .iter()
            .position(|arg| arg == "--after")
            .map(|position| {
                args[position + 1]
                    .strip_prefix("page-")
                    .unwrap()
                    .parse::<u64>()
                    .unwrap()
            })
            .unwrap_or(0);
        // The first 112 entries are discoverable; 62 older entries remain.
        // Model an append to this item's selected notes/gates, not an unrelated
        // project event. Engram refuses the old per-item window in this case.
        if invalidate && index == 14 && !changed.replace(true) {
            return decode_engram_cli_json(
                cli_output(
                    false,
                    "",
                    "catalog changed: selected records changed; start a fresh listing",
                ),
                "evidence notes continuation",
            );
        }
        let mut page = recovery_catalog_page(index, LARGE_TOTAL, PAGE_SIZE);
        // Unrelated project activity can advance this observation while the
        // selected per-item records, counts and continuation stay unchanged.
        page["notes_window"]["read_cut"]["project_position"] = json!(42 + index);
        if index == 0 {
            let mut head = show_receipt(None);
            head["notes"] = page["notes"].clone();
            head["notes_window"] = page["notes_window"].clone();
            head["notes_omitted"] = page["notes_omitted"].clone();
            Ok(head)
        } else {
            Ok(page)
        }
    };
    let mut locators: BTreeSet<String> = (0..3 * PAGE_SIZE)
        .map(|index| format!("{index:032x}"))
        .collect();
    let mut cursor = None;
    let mut complete = false;
    for page in 0..MAX_ACCEPTANCE_BRIEF_EVIDENCE_ENTRIES {
        let mut request = read_request(&key, &format!("large-catalog-{page}"), false);
        request.cursor = cursor.take();
        let result = state
            .read_acceptance_evidence_with_io(&child, request.clone(), &read, || {
                start + Duration::from_millis(calls.load(Ordering::SeqCst) as u64)
            })
            .unwrap();
        if invalidate && changed.get() {
            assert_eq!(locators.len(), 112);
            assert_eq!(result["state"], "unfinished");
            assert_eq!(result["recorded"], false);
            assert!(
                result.get("catalog").is_none(),
                "do not mix an appended catalog into captured evidence"
            );
            let reason = result["unfinishedEvidence"]["reason"].as_str().unwrap();
            assert!(
                reason.contains("notes continuation"),
                "name the failed phase: {reason}"
            );
            assert!(
                reason.contains("catalog changed"),
                "preserve Engram's refusal: {reason}"
            );
            assert!(reason.len() <= 1024);
            let inner = state.inner.lock().unwrap();
            let target = inner.delegations[inner.find_delegation_index(&id).unwrap()]
                .acceptance_evaluation
                .as_ref()
                .unwrap();
            assert!(matches!(
                target.submission,
                AcceptanceEvaluationSubmission::None
            ));
            assert!(acceptance_evaluation_outcome_line(target).contains("infrastructure"));
            return;
        }
        assert_eq!(
            result["state"], "complete",
            "the stable captured catalog must remain readable: {result}"
        );
        for row in result["catalog"].as_array().unwrap() {
            assert!(
                locators.insert(row["locator"].as_str().unwrap().to_owned()),
                "restarting discovery must not expose duplicate records"
            );
        }
        // Successful logical reads replay without another I/O or quota charge.
        let before = calls.load(Ordering::SeqCst);
        let replay = state
            .read_acceptance_evidence_with_io(
                &child,
                request,
                |_, _, _| panic!("cached logical read"),
                std::time::Instant::now,
            )
            .unwrap();
        assert_eq!(result, replay);
        assert_eq!(calls.load(Ordering::SeqCst), before);
        cursor = result["cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            assert_eq!(result["remainingUnread"], 0);
            complete = true;
            break;
        }
    }
    assert!(
        complete,
        "the finite catalog must finish within the original allowance"
    );
    assert_eq!(locators.len(), LARGE_TOTAL as usize);
    if invalidate {
        assert!(
            changed.get(),
            "the invalidating event must actually be reached"
        );
    }
    let inner = state.inner.lock().unwrap();
    let target = inner.delegations[inner.find_delegation_index(&id).unwrap()]
        .acceptance_evaluation
        .as_ref()
        .unwrap();
    assert!(acceptance_evidence_unfinished(target).is_none());
    assert!(matches!(
        target.submission,
        AcceptanceEvaluationSubmission::None
    ));
}

#[test]
fn stable_item_catalog_reaches_all_174_records_despite_project_cut_advances() {
    walk_large_catalog(false);
}

#[test]
fn same_item_append_after_112_records_reports_why_the_remaining_62_were_not_read() {
    walk_large_catalog(true);
}

#[test]
fn exact_criterion_selection_is_issued_before_any_catalog_walk() {
    let (state, _, _, child, key) = recovery_fixture_with_indexed("independent_session", true);
    let result = state
        .read_acceptance_evidence_with_io(
            &child,
            read_request(&key, "selected-exact", true),
            |connection, args, timeout| {
                assert!(!args.iter().any(|arg| arg == "--after"));
                recovery_reader(connection, args, timeout)
            },
            std::time::Instant::now,
        )
        .unwrap();
    assert_eq!(result["state"], "complete");
    assert_eq!(result["records"][0]["note"]["locator"], OLDER_PROOF);
}

#[test]
fn altered_selected_id_refuses_without_io_and_keeps_the_exact_selection_readable() {
    let (state, _, _, child, key) = recovery_fixture_with_indexed("independent_session", true);
    let mut request = read_request(&key, "selected-mismatch", true);
    request.locators[0] = "ffffffffffffffffffffffffffffffff".to_owned();
    let error = state
        .read_acceptance_evidence_with_io(
            &child,
            request,
            |_, _, _| panic!("an unissued id must never reach the store"),
            std::time::Instant::now,
        )
        .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    let exact = state
        .read_acceptance_evidence_with_io(
            &child,
            read_request(&key, "selected-after-mismatch", true),
            recovery_reader,
            std::time::Instant::now,
        )
        .unwrap();
    assert_eq!(exact["state"], "complete");
}

fn infrastructure_failure_preserves_cause(phase: &str, error: EngramTransportError, cause: &str) {
    let (state, _, id, child, key) = recovery_fixture();
    let core_reads = std::cell::Cell::new(0);
    let failed_reads = std::cell::Cell::new(0);
    let result = state
        .read_acceptance_evidence_with_io(
            &child,
            read_request(&key, "infrastructure", false),
            |connection, args, timeout| {
                let at_phase = if args.iter().any(|arg| arg == "inspect") {
                    core_reads.set(core_reads.get() + 1);
                    (phase == "opening inspect" && core_reads.get() == 1)
                        || (phase == "closing inspect" && core_reads.get() == 2)
                } else {
                    phase == "notes continuation" && args.iter().any(|arg| arg == "--after")
                };
                if at_phase {
                    failed_reads.set(failed_reads.get() + 1);
                    Err(error.clone())
                } else {
                    recovery_reader(connection, args, timeout)
                }
            },
            std::time::Instant::now,
        )
        .unwrap();
    assert_eq!(
        failed_reads.get(),
        1,
        "the intended boundary must be reached once"
    );
    assert_eq!(result["state"], "unfinished");
    assert_eq!(result["recorded"], false);
    let reason = result["unfinishedEvidence"]["reason"].as_str().unwrap();
    assert!(
        reason.contains(phase),
        "the failed phase must survive: {reason}"
    );
    assert!(
        reason.contains(cause),
        "the observed cause must survive: {reason}"
    );
    assert!(reason.len() <= 1024);
    let inner = state.inner.lock().unwrap();
    let target = inner.delegations[inner.find_delegation_index(&id).unwrap()]
        .acceptance_evaluation
        .as_ref()
        .unwrap();
    assert!(matches!(
        target.submission,
        AcceptanceEvaluationSubmission::None
    ));
    assert!(
        acceptance_evaluation_outcome_line(target).contains("infrastructure"),
        "requesters must distinguish evaluator infrastructure from a criterion verdict"
    );
}

#[test]
fn recovery_opening_inspect_deadline_retains_phase_and_cause_without_a_verdict() {
    infrastructure_failure_preserves_cause(
        "opening inspect",
        EngramTransportError::deadline("fixture evidence deadline exhausted"),
        "deadline",
    );
}

#[test]
fn recovery_notes_continuation_retains_bounded_cli_stderr_without_a_verdict() {
    infrastructure_failure_preserves_cause(
        "notes continuation",
        decode_engram_cli_json(
            cli_output(false, "", "fixture evidence store unavailable"),
            "evidence read",
        )
        .unwrap_err(),
        "fixture evidence store unavailable",
    );
}

#[test]
fn recovery_closing_inspect_protocol_retains_phase_and_cause_without_a_verdict() {
    infrastructure_failure_preserves_cause(
        "closing inspect",
        EngramTransportError::protocol("fixture evidence protocol malformed response"),
        "protocol",
    );
}

#[test]
fn untrusted_transport_details_stay_bounded_and_do_not_expose_secrets() {
    let (state, _, _, child, key) = recovery_fixture();
    let result = state
        .read_acceptance_evidence_with_io(
            &child,
            read_request(&key, "private-transport", false),
            |connection, args, timeout| {
                if args.iter().any(|arg| arg == "--after") {
                    Err(EngramTransportError::transport(format!(
                        "token=fixture-private-token\u{1b}[31m\n{}",
                        "private diagnostic ".repeat(500)
                    )))
                } else {
                    recovery_reader(connection, args, timeout)
                }
            },
            std::time::Instant::now,
        )
        .unwrap();
    assert_eq!(result["state"], "unfinished");
    let reason = result["unfinishedEvidence"]["reason"].as_str().unwrap();
    assert!(reason.len() <= 1024);
    assert!(!reason.contains("fixture-private-token"));
    assert!(!reason.contains('\u{1b}'));
    assert_eq!(result["recorded"], false);
}
