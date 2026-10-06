// Canonical notes paging across Engram's per-page read cuts, and long note
// histories on a slow store.
//
// Owns: tests that a canonical notes+gates continuation accepts a project
// read cut that advances between pages (Engram returns a fresh cut per call),
// still refuses a total that changes between pages, and that a long history
// read at a realistic per-read latency stops optional paging with a recorded
// stop instead of failing the request.
// Does not own: the other compact-carrier checks (identity, basis, counts,
// cursor), which stay in acceptance_evaluation_evidence.rs, or legacy paging
// limits, which stay in acceptance_evaluation_omissions.rs.
use super::evidence_selection::{
    canonical_binding_receipts, canonical_core_receipt, install_binding_evidence_transport,
};
use super::*;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// One note of a synthetic history, newest first by `index`.
fn history_note(index: u64) -> Value {
    json!({"locator": format!("{index:012x}"), "kind": "generic", "family": "notes",
        "summary": format!("history note {index}")})
}

/// A canonical notes window over `total` notes: `shown` notes after the
/// `newer` newest, cut at `position`, continuing at `page-<next>`.
fn history_window(total: u64, newer: u64, shown: u64, position: i64, next: u64) -> Value {
    let older = total - newer - shown;
    json!({"selection": "newest_first", "order": "oldest_first", "newer": newer,
        "older": older, "shown": shown, "total": total,
        "after": (older > 0).then(|| format!("page-{next}")),
        "includes_gates": true, "read_cut": {"project_position": position,
            "observed_at": format!("2026-10-05T22:00:{:02}Z", next % 60),
            "valid_until_ms": 1_790_000_000_000_i64}})
}

fn history_notes(newer: u64, shown: u64) -> Value {
    json!((newer..newer + shown).map(history_note).collect::<Vec<_>>())
}

/// The canonical initial show over a `total`-note history, `per_page` shown.
fn history_show(total: u64, per_page: u64, position: i64) -> (Value, Value, Value) {
    let (mut show, full, binding) = canonical_binding_receipts();
    show["notes"] = history_notes(0, per_page);
    show["notes_window"] = history_window(total, 0, per_page, position, 1);
    show["notes_omitted"] = json!(total - per_page);
    (show, full, binding)
}

/// The compact continuation `page-<number>`, cut at `position`.
fn history_page(total: u64, per_page: u64, number: u64, position: i64) -> Value {
    let newer = number * per_page;
    let shown = per_page.min(total - newer);
    json!({"work": {"short_ref": "w-task", "title": "Task"},
        "notes": history_notes(newer, shown), "notes_omitted": total - newer - shown,
        "notes_window": history_window(total, newer, shown, position, number + 1)})
}

fn page_number(args: &[String]) -> Option<u64> {
    let token = &args[args.iter().position(|arg| arg == "--after")? + 1];
    token.strip_prefix("page-")?.parse().ok()
}

/// A history whose project read cut advances on every page, as it does while
/// other items in the project are written between the pages.
#[test]
fn canonical_notes_pages_accept_a_read_cut_that_advances_between_pages() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let (show, full, binding) = history_show(6, 2, 120);
    install_binding_evidence_transport(&state, &parent, vec![binding]);
    let pages = AtomicUsize::new(0);
    let response = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(None),
            |_, args, _| {
                if args.iter().any(|arg| arg == "inspect") {
                    Ok(canonical_core_receipt())
                } else if let Some(number) = page_number(args) {
                    pages.fetch_add(1, Ordering::SeqCst);
                    Ok(history_page(6, 2, number, 120 + 9 * number as i64))
                } else if args.iter().any(|arg| arg == "--full") {
                    Ok(full.clone())
                } else if args.iter().any(|arg| arg == "held") {
                    Ok(json!({"items": [], "omitted": 0}))
                } else if args.first().map(String::as_str) == Some("control-policy") {
                    Ok(policy_receipt(Some(&["same_session"])))
                } else {
                    Ok(show.clone())
                }
            },
        )
        .expect("an advancing read cut between pages must not refuse the evaluation");
    assert_eq!(pages.load(Ordering::SeqCst), 2);
    let wire = serde_json::to_value(response).unwrap();
    let brief = wire["brief"].as_str().unwrap();
    for index in 0..6 {
        assert!(
            brief.contains(&format!("{index:012x}")),
            "note {index}: {brief}"
        );
    }
    assert_eq!(wire["evidenceOmissions"]["unread"]["count"], 0);
}

/// The selected index total is still the bracket: an advancing cut does not
/// excuse a page that counts a different history.
#[test]
fn canonical_notes_pages_still_refuse_a_total_that_changes_between_pages() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let (show, full, binding) = history_show(6, 2, 120);
    install_binding_evidence_transport(&state, &parent, vec![binding]);
    let error = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(None),
            |_, args, _| {
                if args.iter().any(|arg| arg == "inspect") {
                    Ok(canonical_core_receipt())
                } else if let Some(number) = page_number(args) {
                    // A note was added to this item between the pages.
                    Ok(history_page(7, 2, number, 121))
                } else if args.iter().any(|arg| arg == "--full") {
                    Ok(full.clone())
                } else if args.iter().any(|arg| arg == "held") {
                    Ok(json!({"items": [], "omitted": 0}))
                } else if args.first().map(String::as_str) == Some("control-policy") {
                    Ok(policy_receipt(Some(&["same_session"])))
                } else {
                    Ok(show.clone())
                }
            },
        )
        .err()
        .expect("a total that changes between pages must be refused");
    assert_eq!(error.status, StatusCode::CONFLICT, "{error:?}");
    assert!(
        error
            .message
            .contains("notes continuation: catalog cut, counts or cursor changed"),
        "{error:?}"
    );
    assert!(state.inner.lock().unwrap().delegations.is_empty());
}

/// What one slow-store request did. `timed_out` counts the reads that decide
/// the request; `optional_timed_out` the optional obligation-assessment reads,
/// whose failure only leaves an assessment incomplete.
struct SlowStoreOutcome {
    result: Result<AcceptanceEvaluationRequestResponse, ApiError>,
    pages: usize,
    timed_out: usize,
    optional_timed_out: usize,
    selected_reads: usize,
    binding_pages: usize,
}

/// The simulated clock of a slow store, shared by the CLI reader and the
/// binding transport: every read takes `latency`, and one funded less than
/// that fails at its timeout with a deadline error, as a real read would.
#[derive(Clone)]
struct SlowStoreClock {
    start: std::time::Instant,
    elapsed_ms: Arc<AtomicU64>,
    latency: Duration,
}

impl SlowStoreClock {
    fn new(latency: Duration) -> Self {
        Self {
            start: std::time::Instant::now(),
            elapsed_ms: Arc::default(),
            latency,
        }
    }

    fn now(&self) -> std::time::Instant {
        self.start + Duration::from_millis(self.elapsed_ms.load(Ordering::SeqCst))
    }

    /// Spends one read's time, or its whole funded timeout when the read
    /// cannot finish within it.
    fn read(&self, timeout: Duration) -> Result<(), EngramTransportError> {
        if self.latency > timeout {
            self.elapsed_ms
                .fetch_add(timeout.as_millis() as u64, Ordering::SeqCst);
            return Err(EngramTransportError::deadline(
                "simulated slow store read timed out",
            ));
        }
        self.elapsed_ms
            .fetch_add(self.latency.as_millis() as u64, Ordering::SeqCst);
        Ok(())
    }
}

/// What the windowed initial show lists of the task's criteria. Engram fits
/// that receipt into its response bound by dropping whole criteria from the
/// end and reporting how many as `acceptance_omitted`; the complete list is
/// only in the `--full` read.
#[derive(Clone, Copy)]
enum VisibleCriteria {
    All,
    First(usize),
    Absent,
}

/// The canonical binding index of a task with `criteria` criteria, in
/// Engram's pages of eight rows; criterion 2 keeps the fixture's binding.
fn binding_pages(criteria: usize) -> Vec<Value> {
    let (_, _, template) = canonical_binding_receipts();
    let bound_row = template["rows"][1].clone();
    let starts = (0..criteria).step_by(8).collect::<Vec<_>>();
    starts
        .iter()
        .enumerate()
        .map(|(page, &earlier)| {
            let shown = 8.min(criteria - earlier);
            let rows = (earlier..earlier + shown)
                .map(|index| {
                    if index == 1 {
                        bound_row.clone()
                    } else {
                        json!({"criterion": index + 1, "binding": null})
                    }
                })
                .collect::<Vec<_>>();
            let omitted = criteria - earlier - shown;
            json!({"basis": template["basis"], "total": criteria, "earlier": earlier,
                "shown": shown, "omitted": omitted, "rows": rows,
                "continuation": (omitted > 0).then(|| format!("binding-{}", page + 2))})
        })
        .collect()
}

/// A binding transport on the slow store's clock that enforces the timeout
/// each binding page is funded.
struct SlowBindingTransport {
    clock: SlowStoreClock,
    pages: Mutex<std::collections::VecDeque<Value>>,
    served: AtomicUsize,
    timed_out: AtomicUsize,
}

impl EngramControlTransport for SlowBindingTransport {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        if let EngramControlRequest::NamedRootSightingRead {
            work_ref,
            run_id,
            run_cut,
        } = request
        {
            return Ok(scripted_no_root_sighting(
                connection, work_ref, run_id, *run_cut,
            ));
        }
        assert!(matches!(
            request,
            EngramControlRequest::AcceptanceBindingRead { .. }
        ));
        if let Err(error) = self.clock.read(timeout) {
            self.timed_out.fetch_add(1, Ordering::SeqCst);
            return Err(error);
        }
        self.served.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .pages
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected extra binding page"))
    }

    fn shutdown_session(&self, _: &str) {}
}

/// The long history: 795 notes. Engram's pages are bounded by bytes, so a
/// history of long notes comes back a few notes a page; three keeps the brief's
/// entry limit out of reach before the page limit.
const LONG_HISTORY_TOTAL: u64 = 795;
const LONG_HISTORY_PER_PAGE: u64 = 3;

/// The long history on a store where every read takes `latency` against a
/// simulated clock, for a task with `criteria` criteria (more than eight make
/// the binding index page). The CLI reads and the binding pages share the
/// clock, and each fails at its funded timeout when it cannot finish in it.
fn long_history_on_a_slow_store(
    latency: Duration,
    selected: &[&str],
    criteria: usize,
    visible: VisibleCriteria,
) -> SlowStoreOutcome {
    const TOTAL: u64 = LONG_HISTORY_TOTAL;
    const PER_PAGE: u64 = LONG_HISTORY_PER_PAGE;
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let (mut show, mut full, binding) = history_show(TOTAL, PER_PAGE, 5_000);
    let acceptance = (1..=criteria)
        .map(|criterion| format!("Criterion {criterion} holds"))
        .collect::<Vec<_>>();
    match visible {
        VisibleCriteria::All => show["status"]["work"]["acceptance"] = json!(acceptance),
        VisibleCriteria::First(shown) => {
            show["status"]["work"]["acceptance"] = json!(acceptance[..shown]);
            show["status"]["work"]["acceptance_omitted"] = json!(criteria - shown);
        }
        VisibleCriteria::Absent => {
            show["status"]["work"]
                .as_object_mut()
                .unwrap()
                .remove("acceptance");
        }
    }
    full["work"]["acceptance"] = json!(acceptance);
    // Routing for canonical discovery, then the slow transport in its place.
    install_binding_evidence_transport(&state, &parent, vec![binding]);
    let clock = SlowStoreClock::new(latency);
    let bindings = Arc::new(SlowBindingTransport {
        clock: clock.clone(),
        pages: Mutex::new(binding_pages(criteria).into()),
        served: AtomicUsize::new(0),
        timed_out: AtomicUsize::new(0),
    });
    state.install_test_engram_transport(bindings.clone());
    let pages = AtomicUsize::new(0);
    let timed_out = AtomicUsize::new(0);
    let optional_timed_out = AtomicUsize::new(0);
    let selected_reads = AtomicUsize::new(0);
    let request = serde_json::from_value(json!({"workRef": "w-task", "criterionEvidence":
        if selected.is_empty() { json!([]) } else { json!([{"criterion": 1, "locators": selected}]) }}))
    .unwrap();
    let result = state.request_acceptance_evaluation_until(
        &parent,
        request,
        |_, args, timeout| {
            if let Err(error) = clock.read(timeout) {
                // The binding's closure record is also read whole, by the
                // optional obligation-assessment reader; it decides nothing.
                let optional = args
                    .iter()
                    .position(|arg| arg == "--note")
                    .is_some_and(|at| !selected.contains(&args[at + 1].as_str()));
                if optional {
                    &optional_timed_out
                } else {
                    &timed_out
                }
                .fetch_add(1, Ordering::SeqCst);
                return Err(error);
            }
            if args.iter().any(|arg| arg == "inspect") {
                Ok(canonical_core_receipt())
            } else if let Some(number) = page_number(args) {
                pages.fetch_add(1, Ordering::SeqCst);
                // A steady cut keeps this fixture about time alone; the
                // advancing cut has its own test above.
                Ok(history_page(TOTAL, PER_PAGE, number, 5_000))
            } else if let Some(position) = args.iter().position(|arg| arg == "--note") {
                // The binding's closure record is also read whole, by the
                // optional obligation-assessment reader; count the selection.
                if selected.contains(&args[position + 1].as_str()) {
                    selected_reads.fetch_add(1, Ordering::SeqCst);
                }
                Ok(
                    json!({"work_ref": "w-task", "note": {"locator": args[position + 1],
                    "kind": "generic", "family": "notes", "summary": "Selected older proof"}}),
                )
            } else if args.iter().any(|arg| arg == "--full") {
                Ok(full.clone())
            } else if args.iter().any(|arg| arg == "held") {
                Ok(json!({"items": [], "omitted": 0}))
            } else if args.first().map(String::as_str) == Some("control-policy") {
                Ok(policy_receipt(Some(&["same_session"])))
            } else {
                Ok(show.clone())
            }
        },
        clock.now() + acceptance_evaluation_request_tracker_budget(),
        || clock.now(),
    );
    SlowStoreOutcome {
        result,
        pages: pages.load(Ordering::SeqCst),
        timed_out: timed_out.load(Ordering::SeqCst) + bindings.timed_out.load(Ordering::SeqCst),
        optional_timed_out: optional_timed_out.load(Ordering::SeqCst),
        selected_reads: selected_reads.load(Ordering::SeqCst),
        binding_pages: bindings.served.load(Ordering::SeqCst),
    }
}

fn assert_paging_stopped_on_time_and_evaluated(outcome: SlowStoreOutcome) -> Value {
    let response = outcome
        .result
        .expect("a long history on a slow store must still evaluate, not fail with 502 or 409");
    // Optional assessment reads may run out of time; they only leave an
    // assessment incomplete, and are counted apart.
    let _ = outcome.optional_timed_out;
    assert_eq!(
        outcome.timed_out, 0,
        "no read that decides the request may outrun the time it was funded"
    );
    let wire = serde_json::to_value(response).unwrap();
    let unread = &wire["evidenceOmissions"]["unread"];
    assert_eq!(unread["reason"], "time_budget", "{unread}");
    assert_eq!(
        unread["count"].as_u64(),
        Some(LONG_HISTORY_TOTAL - LONG_HISTORY_PER_PAGE * (outcome.pages as u64 + 1)),
        "{unread}"
    );
    let brief = wire["brief"].as_str().unwrap();
    assert!(brief.contains("paging stopped: time_budget"), "{brief}");
    wire
}

/// At four seconds a read, paging the history to its page limit would leave a
/// page with less time than it takes. Paging stops first, and says so.
#[test]
fn a_long_note_history_on_a_slow_store_stops_paging_and_still_evaluates() {
    let outcome =
        long_history_on_a_slow_store(Duration::from_secs(4), &[], 2, VisibleCriteria::All);
    let (pages, selected_reads, binding_pages) =
        (outcome.pages, outcome.selected_reads, outcome.binding_pages);
    assert_paging_stopped_on_time_and_evaluated(outcome);
    assert!(pages >= 1, "the share that can fund pages is used");
    assert_eq!(selected_reads, 0);
    assert_eq!(binding_pages, 1);
}

/// More than eight criteria page the binding index. Every binding page is a
/// read that decides the request, so optional paging leaves time for each.
#[test]
fn a_long_note_history_keeps_every_binding_page_funded() {
    let outcome =
        long_history_on_a_slow_store(Duration::from_secs(4), &[], 10, VisibleCriteria::All);
    let (pages, binding_pages) = (outcome.pages, outcome.binding_pages);
    assert_paging_stopped_on_time_and_evaluated(outcome);
    assert!(pages >= 1, "the share that can fund pages is used");
    assert_eq!(binding_pages, 2);
}

/// A windowed show that clipped its criteria list still counts the clipped
/// criteria: the binding index pages over all of them.
#[test]
fn a_clipped_criteria_list_still_funds_every_binding_page() {
    let outcome =
        long_history_on_a_slow_store(Duration::from_secs(4), &[], 10, VisibleCriteria::First(3));
    let (pages, binding_pages) = (outcome.pages, outcome.binding_pages);
    assert_paging_stopped_on_time_and_evaluated(outcome);
    assert!(pages >= 1, "the share that can fund pages is used");
    assert_eq!(binding_pages, 2);
}

/// Without a criteria list the binding index size is unknown, so optional
/// paging keeps the largest binding reserve and the request still evaluates.
#[test]
fn an_absent_criteria_list_keeps_the_binding_index_funded() {
    let outcome =
        long_history_on_a_slow_store(Duration::from_secs(4), &[], 10, VisibleCriteria::Absent);
    let binding_pages = outcome.binding_pages;
    assert_paging_stopped_on_time_and_evaluated(outcome);
    assert_eq!(binding_pages, 2);
}

/// Optional paging never takes the share of the selected-evidence reads, which
/// decide the request.
#[test]
fn a_long_note_history_keeps_the_selected_evidence_reads_funded() {
    let selected = [
        "11111111111111111111111111111111",
        "22222222222222222222222222222222",
        "33333333333333333333333333333333",
    ];
    let outcome =
        long_history_on_a_slow_store(Duration::from_secs(3), &selected, 2, VisibleCriteria::All);
    let (pages, selected_reads) = (outcome.pages, outcome.selected_reads);
    let wire = assert_paging_stopped_on_time_and_evaluated(outcome);
    assert!(pages >= 1, "the share that can fund pages is used");
    assert_eq!(selected_reads, selected.len());
    assert_eq!(wire["criterionEvidence"][0]["locators"], json!(selected));
}
