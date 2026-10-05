// Obligation assessments of verification records in the acceptance evaluator's
// brief and the evaluation request result.
//
// Owns: reading a verification record's whole obligation assessment through
// the tracker's `show --note` and its `--after` continuations, in either shape
// the tracker serves (the old row listing, which its history view keeps, or
// its summary view of counts and must_show rows); the compact summary of it
// (the record and cut positions, one count per status group, the rows to list
// in full, and the command for the full history at the same cut); fitting
// those summaries into the room a finished
// brief leaves, whole or named as not carried; and the requester's notice of
// any assessment that was not read whole or not carried.
// Does not own: which records the brief carries or how they are read
// (acceptance_evidence_selection.rs), the brief's own sections and the order
// in which they shrink (acceptance_evaluation.rs), or the tracker's own
// `show --note` text, which holders read through the tracker and TermAl passes
// through untouched.
// Split from: new code, included from acceptance_evidence_selection.rs beside
// the exact record reads it extends.

/// At most this many pages of one record's assessment are read. The tracker
/// gives 8 rows a page, so 32 pages read 256 rows.
const MAX_ACCEPTANCE_ASSESSMENT_PAGES_PER_RECORD: usize = 32;

/// At most this many assessment pages are read for one evaluation request,
/// beyond the first pages the exact record reads already returned.
const MAX_ACCEPTANCE_ASSESSMENT_PAGES: usize = 96;

/// A row field longer than this is not rendered: the summary is marked
/// incomplete instead, so that no row is ever shown cut.
const MAX_ACCEPTANCE_ASSESSMENT_FIELD_CHARS: usize = 256;

/// Where the independent brief's assessment section goes: before the evidence
/// list, after the criterion evidence index.
const ACCEPTANCE_ASSESSMENT_ANCHOR: &str = "Evidence recorded on the task";

/// Where the same-session brief's assessment section goes.
const ACCEPTANCE_SAME_SESSION_ASSESSMENT_ANCHOR: &str = "Evidence not shown in this brief:";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceObligationGroup {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recorded: Option<String>,
    count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceObligationRow {
    status: String,
    /// The mismatch or left-out reason the tracker gave the row.
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    rule: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    rule_version: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    criterion: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    check_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recorded: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trigger_position: Option<u64>,
    pinned: bool,
}

/// One verification record's obligation assessment as the evaluator gets it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceObligationAssessment {
    locator: String,
    /// The tracker's row count, when a page was read.
    total: Option<u64>,
    /// The rows read: every row of the old shape, or the must_show rows of
    /// the summary view.
    rows_read: u64,
    /// The summary view's must_show row count; absent for the old shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    must_show_total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    record_position: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cut_position: Option<u64>,
    /// Every row was read, at one cut.
    complete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    incomplete_reason: Option<String>,
    /// The tracker's own continuation where reading stopped, verbatim; it
    /// binds the captured cut.
    #[serde(skip_serializing_if = "Option::is_none")]
    resume: Option<String>,
    /// When `resume` is the cursor that fetched the last page read, which
    /// page and rows resuming reads again.
    #[serde(skip_serializing_if = "Option::is_none")]
    resume_rereads: Option<AcceptanceAssessmentReread>,
    /// The first page's own continuation, verbatim: with `full_history` for
    /// page 1, it pages the rest of the history at the captured cut. Absent
    /// when the first page named none that can be quoted.
    #[serde(skip_serializing_if = "Option::is_none")]
    history_continuation: Option<String>,
    /// The cursorless command for the first page. It reads the record's
    /// assessment as it stands now, not at the captured cut.
    full_history: String,
    /// One count per (status, reason, recorded) group of the rows read.
    groups: Vec<AcceptanceObligationGroup>,
    /// In full: every row read that another record did not close (old shape),
    /// or every must_show row read (summary view).
    rows: Vec<AcceptanceObligationRow>,
    /// The evaluator's brief carries this summary.
    carried: bool,
}

/// A page that resuming at the cursor which fetched it reads again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceAssessmentReread {
    /// One-based page number in reading order.
    page: u64,
    /// One-based first and last row of that page.
    first_row: u64,
    last_row: u64,
}

/// The `recorded` end of a row whose obligation another record satisfied.
const ACCEPTANCE_OBLIGATION_CLOSED_BY_ANOTHER: &str = "satisfied_by_another_record";

/// A row another record closed: counted in its group, never listed. The
/// tracker's `status` says how this record met the obligation (`matches`,
/// `mismatch`, or `left_out`, skipped before matching); whether the obligation
/// is closed is its `recorded` end, so a left-out row that is still open is
/// listed like any other. A mismatch is always listed, whatever its end: a
/// failed check is what a reader must see, even once another record closed
/// the obligation.
fn acceptance_obligation_row_is_closed(status: &str, recorded: Option<&str>) -> bool {
    status != "mismatch" && recorded == Some(ACCEPTANCE_OBLIGATION_CLOSED_BY_ANOTHER)
}

/// Groups list the rows that need attention first: obligations still open,
/// then mismatches, then what this record satisfied, then the rest, and the
/// rows another record closed last.
fn acceptance_obligation_group_rank(status: &str, recorded: Option<&str>) -> u8 {
    if acceptance_obligation_row_is_closed(status, recorded) {
        4
    } else if recorded == Some("open") {
        0
    } else {
        match status {
            "mismatch" => 1,
            "matches" => 2,
            _ => 3,
        }
    }
}

/// A text field of an assessment row: absent, or a bounded single-line word.
/// `Err` marks a row that cannot be shown whole.
fn acceptance_assessment_text(row: &Value, key: &str) -> Result<Option<String>, ()> {
    match row.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text))
            if text.chars().count() <= MAX_ACCEPTANCE_ASSESSMENT_FIELD_CHARS
                && !text.chars().any(char::is_control) =>
        {
            Ok(Some(text.clone()))
        }
        Some(_) => Err(()),
    }
}

fn acceptance_assessment_number(row: &Value, key: &str) -> Result<Option<u64>, ()> {
    match row.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or(()),
    }
}

fn acceptance_obligation_row(row: &Value) -> Result<AcceptanceObligationRow, ()> {
    let status = acceptance_assessment_text(row, "status")?.ok_or(())?;
    let rule = acceptance_assessment_text(row, "rule")?.ok_or(())?;
    let reason = match acceptance_assessment_text(row, "mismatch")? {
        Some(reason) => Some(reason),
        None => match acceptance_assessment_text(row, "left_out")? {
            Some(reason) => Some(reason),
            None => acceptance_assessment_text(row, "reason")?,
        },
    };
    let pinned = match row.get("pinned") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(pinned)) => *pinned,
        Some(_) => return Err(()),
    };
    Ok(AcceptanceObligationRow {
        status,
        reason,
        rule,
        rule_version: acceptance_assessment_number(row, "rule_version")?,
        criterion: acceptance_assessment_number(row, "criterion")?,
        check_kind: acceptance_assessment_text(row, "check_kind")?,
        recorded: acceptance_assessment_text(row, "recorded")?,
        trigger_position: acceptance_assessment_number(row, "trigger_position")?,
        pinned,
    })
}

/// The `--after` token of a tracker continuation for `locator`, or none when
/// the continuation is not one bounded line of plain text or does not name
/// this record's assessment. Only a continuation with a token is quoted back
/// as a resume point.
fn acceptance_assessment_continuation_token(continuation: &str, locator: &str) -> Option<String> {
    if continuation.len() > MAX_ACCEPTANCE_CONTINUATION_BYTES
        || continuation.chars().any(char::is_control)
    {
        return None;
    }
    let words = continuation.split_whitespace().collect::<Vec<_>>();
    let note = words.iter().position(|word| *word == "--note")?;
    let named = words.get(note + 1)?;
    if named.len() < 8 || !locator.starts_with(named) {
        return None;
    }
    let after = words.iter().position(|word| *word == "--after")?;
    let token = words.get(after + 1)?;
    (!token.starts_with("--") && token.len() <= MAX_ACCEPTANCE_CONTINUATION_BYTES)
        .then(|| (*token).to_owned())
}

fn acceptance_assessment_read_failure(error: &EngramTransportError) -> &'static str {
    if matches!(error.kind, EngramTransportErrorKind::Deadline) {
        "time_budget"
    } else if error.message.contains("maximum control frame") {
        "frame_bound"
    } else {
        "transport_failure"
    }
}

/// The shape a tracker serves an assessment page in. The history view keeps
/// the old shape; the summary view carries counts and the must_show rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AcceptanceAssessmentShape {
    Rows,
    Summary,
}

fn acceptance_assessment_shape(page: &Value) -> Result<AcceptanceAssessmentShape, &'static str> {
    match page.get("view") {
        None => Ok(AcceptanceAssessmentShape::Rows),
        Some(Value::String(view)) if view == "history" => Ok(AcceptanceAssessmentShape::Rows),
        Some(Value::String(view)) if view == "summary" => Ok(AcceptanceAssessmentShape::Summary),
        Some(_) => Err("malformed_page"),
    }
}

/// One `counts` entry of the summary view: a status, a non-negative count,
/// and a reason that is a plain string when present.
fn acceptance_obligation_count(entry: &Value) -> Result<AcceptanceObligationGroup, ()> {
    let status = acceptance_assessment_text(entry, "status")?.ok_or(())?;
    let reason = match entry.get("reason") {
        None => None,
        Some(Value::String(_)) => acceptance_assessment_text(entry, "reason")?,
        Some(_) => return Err(()),
    };
    let count = entry.get("count").and_then(Value::as_u64).ok_or(())?;
    Ok(AcceptanceObligationGroup {
        status,
        reason,
        recorded: None,
        count,
    })
}

/// Accumulates one record's assessment pages at one cut.
struct AcceptanceAssessmentPages {
    assessment: AcceptanceObligationAssessment,
    counts: BTreeMap<(u8, String, Option<String>, Option<String>), u64>,
    /// The shape of the first page; one record's read never mixes shapes.
    shape: Option<AcceptanceAssessmentShape>,
    /// The summary view's counts, the same on every page of one cut.
    summary_groups: Option<Vec<AcceptanceObligationGroup>>,
    /// Pages accepted so far.
    pages_read: u64,
    /// The last page accepted.
    last_page: Option<AcceptanceAssessmentReread>,
}

impl AcceptanceAssessmentPages {
    fn new(locator: &str, work_ref: &str) -> Self {
        Self {
            assessment: AcceptanceObligationAssessment {
                locator: locator.to_owned(),
                total: None,
                rows_read: 0,
                must_show_total: None,
                record_position: None,
                cut_position: None,
                complete: false,
                incomplete_reason: None,
                resume: None,
                resume_rereads: None,
                history_continuation: None,
                full_history: format!("engram work show {work_ref} --note {locator} --json"),
                groups: Vec::new(),
                rows: Vec::new(),
                carried: false,
            },
            counts: BTreeMap::new(),
            shape: None,
            summary_groups: None,
            pages_read: 0,
            last_page: None,
        }
    }

    /// Adds one page in either shape; `Err` names why the page cannot extend
    /// the rows read. A page in another shape than the first is malformed.
    fn add(&mut self, page: &Value) -> Result<(), &'static str> {
        let shape = acceptance_assessment_shape(page)?;
        if *self.shape.get_or_insert(shape) != shape {
            return Err("malformed_page");
        }
        match shape {
            AcceptanceAssessmentShape::Rows => self.add_rows(page),
            AcceptanceAssessmentShape::Summary => self.add_summary(page),
        }
    }

    /// Holds every page to the first page's row count and positions: one cut.
    fn same_cut(
        &mut self,
        total: u64,
        record_position: Option<u64>,
        cut_position: Option<u64>,
    ) -> Result<(), &'static str> {
        if self.assessment.total.is_none() {
            self.assessment.total = Some(total);
            self.assessment.record_position = record_position;
            self.assessment.cut_position = cut_position;
            Ok(())
        } else if self.assessment.total != Some(total)
            || self.assessment.record_position != record_position
            || self.assessment.cut_position != cut_position
        {
            Err("assessment_changed")
        } else {
            Ok(())
        }
    }

    /// Records the page just accepted, and the first page's same-cut
    /// continuation for the full history: the old shape's own continuation,
    /// or the summary view's `history` command.
    fn accepted(&mut self, page: &Value, earlier: u64, history_key: &str) {
        self.pages_read += 1;
        self.last_page = Some(AcceptanceAssessmentReread {
            page: self.pages_read,
            first_row: earlier + 1,
            last_row: self.assessment.rows_read,
        });
        if self.pages_read == 1 {
            self.assessment.history_continuation = page
                .get(history_key)
                .and_then(Value::as_str)
                .filter(|continuation| {
                    acceptance_assessment_continuation_token(continuation, &self.assessment.locator)
                        .is_some()
                })
                .map(str::to_owned);
        }
    }

    /// Adds one summary-view page. Its counts cover every row and are the
    /// same on each page; its must_show rows are listed in full, page by page
    /// from where the rows read end. Every number must be a present
    /// non-negative integer, `counts` and `must_show` arrays with one count
    /// per status and reason, the counts must sum to the total, the must_show
    /// total may not exceed it, and the page's must_show offsets must add up
    /// to the must_show total; a page that does not say so is malformed. The
    /// whole page is checked before anything of it is kept, so a page rejected
    /// for any reason leaves no count, total or row behind.
    fn add_summary(&mut self, page: &Value) -> Result<(), &'static str> {
        let number = |key: &str| page.get(key).and_then(Value::as_u64).ok_or("malformed_page");
        let total = number("total")?;
        let record_position = Some(number("record_position")?);
        let cut_position = Some(number("cut_position")?);
        let must_show_total = number("must_show_total")?;
        let earlier = number("must_show_earlier")?;
        let remaining = number("must_show_remaining")?;
        let mut groups = page
            .get("counts")
            .and_then(Value::as_array)
            .ok_or("malformed_page")?
            .iter()
            .map(acceptance_obligation_count)
            .collect::<Result<Vec<_>, ()>>()
            .map_err(|()| "malformed_page")?;
        // One count per status and reason, compared as a set across pages.
        groups.sort_by(|left, right| (&left.status, &left.reason).cmp(&(&right.status, &right.reason)));
        if groups
            .windows(2)
            .any(|pair| (&pair[0].status, &pair[0].reason) == (&pair[1].status, &pair[1].reason))
        {
            return Err("malformed_page");
        }
        let rows = page.get("must_show").and_then(Value::as_array).ok_or("malformed_page")?;
        let counted = groups
            .iter()
            .try_fold(0_u64, |sum, group| sum.checked_add(group.count))
            .ok_or("malformed_page")?;
        let shown = u64::try_from(rows.len()).map_err(|_| "malformed_page")?;
        let paged = earlier
            .checked_add(shown)
            .and_then(|sum| sum.checked_add(remaining))
            .ok_or("malformed_page")?;
        if counted != total || must_show_total > total || paged != must_show_total {
            return Err("malformed_page");
        }
        // The same cut as the pages already read: totals, positions, counts,
        // and an offset that starts where the rows read end (no gap, no repeat).
        let changed = self.assessment.total.is_some_and(|known| known != total)
            || self.assessment.total.is_some()
                && (self.assessment.record_position != record_position
                    || self.assessment.cut_position != cut_position)
            || self.assessment.must_show_total.is_some_and(|known| known != must_show_total)
            || self.summary_groups.as_ref().is_some_and(|known| *known != groups)
            || earlier != self.assessment.rows_read;
        if changed {
            return Err("assessment_changed");
        }
        let parsed = rows
            .iter()
            .map(acceptance_obligation_row)
            .collect::<Result<Vec<_>, ()>>()
            .map_err(|()| "malformed_row")?;
        // Every check passed: only now is the page kept.
        self.same_cut(total, record_position, cut_position)?;
        self.assessment.must_show_total = Some(must_show_total);
        self.summary_groups = Some(groups);
        self.assessment.rows_read += shown;
        self.assessment.rows.extend(parsed);
        self.accepted(page, earlier, "history");
        Ok(())
    }

    /// Adds one old-shape page. The count, the offset and both positions are
    /// what make a read whole at one cut, so each must be a present
    /// non-negative integer, and the rows an array; a page that does not say
    /// so is malformed.
    fn add_rows(&mut self, page: &Value) -> Result<(), &'static str> {
        let number = |key: &str| page.get(key).and_then(Value::as_u64).ok_or("malformed_page");
        let total = number("total")?;
        let earlier = number("earlier")?;
        let record_position = Some(number("record_position")?);
        let cut_position = Some(number("cut_position")?);
        let rows = page.get("rows").and_then(Value::as_array).ok_or("malformed_page")?;
        self.same_cut(total, record_position, cut_position)?;
        // Each page must start where the rows read end: no gap, no repeat.
        if earlier != self.assessment.rows_read
            || self.assessment.rows_read + rows.len() as u64 > total
        {
            return Err("assessment_changed");
        }
        let parsed = rows
            .iter()
            .map(acceptance_obligation_row)
            .collect::<Result<Vec<_>, ()>>()
            .map_err(|()| "malformed_row")?;
        for row in parsed {
            *self
                .counts
                .entry((
                    acceptance_obligation_group_rank(&row.status, row.recorded.as_deref()),
                    row.status.clone(),
                    row.reason.clone(),
                    row.recorded.clone(),
                ))
                .or_default() += 1;
            self.assessment.rows_read += 1;
            if !acceptance_obligation_row_is_closed(&row.status, row.recorded.as_deref()) {
                self.assessment.rows.push(row);
            }
        }
        // The first page's own continuation pages the rest at this cut.
        self.accepted(page, earlier, "continuation");
        Ok(())
    }

    /// Every row to be read has been: every row of the old shape, or every
    /// must_show row of the summary view.
    fn read_whole(&self) -> bool {
        match self.assessment.must_show_total {
            Some(must_show) => self.assessment.rows_read == must_show,
            None => self.assessment.total == Some(self.assessment.rows_read),
        }
    }

    /// Finishes at `resume`, the cursor that fetched the last page accepted:
    /// resuming there reads that page again, and the summary says which.
    fn finish_rereading(
        self,
        stopped: &'static str,
        resume: Option<String>,
    ) -> AcceptanceObligationAssessment {
        let rereads = resume.as_ref().and(self.last_page);
        let mut assessment = self.finish(Some(stopped), resume);
        assessment.resume_rereads = rereads;
        assessment
    }

    fn finish(
        mut self,
        stopped: Option<&'static str>,
        resume: Option<String>,
    ) -> AcceptanceObligationAssessment {
        let read_whole = self.read_whole();
        let reason = stopped.or((!read_whole).then_some("missing_continuation"));
        self.assessment.complete = reason.is_none();
        self.assessment.incomplete_reason = reason.map(str::to_owned);
        self.assessment.resume = reason.and(resume);
        self.assessment.groups = match self.summary_groups {
            // The summary view's counts, in the same order of attention.
            Some(mut groups) => {
                groups.sort_by(|left, right| {
                    let rank = |group: &AcceptanceObligationGroup| {
                        acceptance_obligation_group_rank(&group.status, None)
                    };
                    (rank(left), &left.status, &left.reason)
                        .cmp(&(rank(right), &right.status, &right.reason))
                });
                groups
            }
            None => self
                .counts
                .into_iter()
                .map(|((_, status, reason, recorded), count)| AcceptanceObligationGroup {
                    status,
                    reason,
                    recorded,
                    count,
                })
                .collect(),
        };
        self.assessment
    }
}

/// The verification records the brief's criterion evidence index cites, in
/// index order, whether the brief has them from an exact read, a closure
/// projection or the newest-notes window.
fn acceptance_assessment_locators(task: &AcceptanceEvaluationTask) -> Vec<String> {
    let is_verification = |locator: &str| {
        task.indexed_evidence
            .iter()
            .chain(task.evidence.iter())
            .find(|entry| entry.locator == locator)
            .is_some_and(|entry| entry.kind == "verification" && !entry.non_holder)
    };
    let mut seen = BTreeSet::new();
    task.criterion_evidence
        .iter()
        .flat_map(|link| link.locators.iter())
        .chain(task.indexed_evidence.iter().map(|entry| &entry.locator))
        .filter(|locator| is_verification(locator))
        .filter(|locator| seen.insert((*locator).clone()))
        .cloned()
        .collect()
}

/// The first assessment page of an exact `show --note` receipt for `locator`:
/// `Ok(None)` when that verification record has none, `Err` when the receipt
/// is not that holder verification record.
fn acceptance_assessment_first_page(
    result: &Value,
    locator: &str,
) -> Result<Option<Value>, &'static str> {
    let note = result.get("note").filter(|note| note.is_object()).ok_or("invalid_receipt")?;
    if note.get("locator").and_then(Value::as_str) != Some(locator)
        || note.get("kind").and_then(Value::as_str) != Some("verification")
        || note.get("non_holder").and_then(Value::as_bool) == Some(true)
    {
        return Err("invalid_receipt");
    }
    match note.get("assessment") {
        None | Some(Value::Null) => Ok(None),
        Some(page) if page.is_object() => Ok(Some(page.clone())),
        Some(_) => Err("malformed_page"),
    }
}

/// Reads the whole obligation assessment of every verification record the
/// brief's criterion evidence index cites (the requester's selected records
/// and the original closures, wherever the brief has their bodies from),
/// within `deadline`. A record whose assessment cannot be read whole gets an
/// incomplete summary that says why and where to resume; a read failure here
/// never fails the request. Records without an assessment get none.
fn read_acceptance_obligation_assessments(
    task: &mut AcceptanceEvaluationTask,
    connection: &EngramConnectionConfig,
    show_args: &[String],
    read: &impl Fn(&EngramConnectionConfig, &[String], Duration) -> Result<Value, EngramTransportError>,
    deadline: std::time::Instant,
    now: &impl Fn() -> std::time::Instant,
) {
    let mut note_args = show_args.to_vec();
    if let Some(notes) = note_args.iter().position(|arg| arg == "--notes") {
        note_args.truncate(notes);
    }
    let locators = acceptance_assessment_locators(task);
    let first_pages = std::mem::take(&mut task.assessment_first_pages);
    let work_ref = task.work_ref.clone();
    let mut budget = MAX_ACCEPTANCE_ASSESSMENT_PAGES;
    for locator in locators {
        let read_page = |after: Option<&str>| {
            let mut args = note_args.clone();
            args.extend(["--note".to_owned(), locator.clone()]);
            if let Some(token) = after {
                args.extend(["--after".to_owned(), token.to_owned()]);
            }
            args.push("--json".to_owned());
            let timeout = acceptance_criterion_evidence_read_timeout(deadline, now)
                .map_err(|_| "time_budget")?;
            let result = read(connection, &args, timeout)
                .map_err(|error| acceptance_assessment_read_failure(&error))?;
            if result.get("work_ref").and_then(Value::as_str) != Some(work_ref.as_str()) {
                return Err("invalid_receipt");
            }
            Ok(result)
        };
        let mut pages = AcceptanceAssessmentPages::new(&locator, &work_ref);
        // An exact selected read already said whether the record has one.
        let first = match first_pages.iter().find(|(read, _)| *read == locator) {
            Some((_, Value::Null)) => Ok(None),
            Some((_, page)) if page.is_object() => Ok(Some(page.clone())),
            Some(_) => Err("malformed_page"),
            None if budget == 0 => Err("page_limit"),
            None => {
                budget -= 1;
                read_page(None)
                    .and_then(|result| acceptance_assessment_first_page(&result, &locator))
            }
        };
        let mut page = match first {
            // A record without an assessment keeps its brief unchanged.
            Ok(None) => continue,
            Ok(Some(page)) => page,
            Err(reason) => {
                task.obligation_assessments.push(pages.finish(Some(reason), None));
                continue;
            }
        };
        let mut pages_read = 0;
        // The continuation that fetched `page`: where a reader resumes when
        // the host rejects that page.
        let mut fetched_by: Option<String> = None;
        // The third element says `resume` is the cursor that fetched the last
        // page accepted, so resuming reads that page again.
        let (stopped, resume, rereads) = loop {
            if let Err(reason) = pages.add(&page) {
                break (Some(reason), fetched_by, false);
            }
            pages_read += 1;
            // Every row is read: the read is whole, and a continuation the
            // last page still carries is not followed.
            if pages.read_whole() {
                break (None, None, false);
            }
            // The read is not whole and names no next page this host can
            // follow: the cursor that fetched this page is the last point at
            // the captured cut, if there is one.
            let Some(token) = page
                .get("continuation")
                .and_then(Value::as_str)
                .and_then(|continuation| acceptance_assessment_continuation_token(continuation, &locator))
            else {
                break (Some("missing_continuation"), fetched_by, true);
            };
            let resume = page
                .get("continuation")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if pages_read >= MAX_ACCEPTANCE_ASSESSMENT_PAGES_PER_RECORD || budget == 0 {
                break (Some("page_limit"), resume, false);
            }
            budget -= 1;
            let next = read_page(Some(&token)).and_then(|result| {
                if result.get("locator").and_then(Value::as_str) != Some(locator.as_str()) {
                    return Err("invalid_receipt");
                }
                match result.get("assessment") {
                    Some(next) if next.is_object() => Ok(next.clone()),
                    _ => Err("malformed_page"),
                }
            });
            match next {
                Ok(next) => {
                    page = next;
                    fetched_by = resume;
                }
                Err(reason) => break (Some(reason), resume, false),
            }
        };
        task.obligation_assessments.push(match stopped {
            Some(reason) if rereads => pages.finish_rereading(reason, resume),
            _ => pages.finish(stopped, resume),
        });
    }
}

fn acceptance_obligation_group_label(group: &AcceptanceObligationGroup) -> String {
    let detail = [group.reason.as_deref(), group.recorded.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if detail.is_empty() {
        format!("{} {}", group.count, group.status)
    } else {
        format!("{} {} ({})", group.count, group.status, detail.join("; "))
    }
}

fn acceptance_obligation_row_line(row: &AcceptanceObligationRow) -> String {
    let mut parts = vec![match row.rule_version {
        Some(version) => format!("{} v{version}", row.rule),
        None => row.rule.clone(),
    }];
    if let Some(criterion) = row.criterion {
        parts.push(format!("criterion {criterion}"));
    }
    if let Some(check_kind) = &row.check_kind {
        parts.push(format!("check {check_kind}"));
    }
    if let Some(recorded) = &row.recorded {
        parts.push(format!("recorded {recorded}"));
    }
    if let Some(trigger) = row.trigger_position {
        parts.push(format!("trigger {trigger}"));
    }
    if row.pinned {
        parts.push("pinned".to_owned());
    }
    let status = match &row.reason {
        Some(reason) => format!("{} ({reason})", row.status),
        None => row.status.clone(),
    };
    format!("  - obligation {status}: {}", parts.join(", "))
}

fn acceptance_obligation_total(assessment: &AcceptanceObligationAssessment) -> String {
    assessment
        .total
        .map_or_else(|| "an unknown number of".to_owned(), |total| total.to_string())
}

/// How much of an incomplete read was read: rows of the total, or must_show
/// rows of the must_show total in the summary view.
fn acceptance_obligation_read_extent(assessment: &AcceptanceObligationAssessment) -> String {
    match assessment.must_show_total {
        Some(must_show) => format!(
            "{} of {must_show} must_show rows read, of {} rows",
            assessment.rows_read,
            acceptance_obligation_total(assessment)
        ),
        None => format!(
            "{} of {} rows read",
            assessment.rows_read,
            acceptance_obligation_total(assessment)
        ),
    }
}

/// The size of a record read whole.
fn acceptance_obligation_whole_extent(assessment: &AcceptanceObligationAssessment) -> String {
    match assessment.must_show_total {
        Some(must_show) => format!(
            "{} rows, {must_show} must_show",
            acceptance_obligation_total(assessment)
        ),
        None => format!("{} rows", acceptance_obligation_total(assessment)),
    }
}

/// What resuming at `resume` reads again, when it is the cursor that fetched
/// the last page read.
fn acceptance_obligation_reread_text(assessment: &AcceptanceObligationAssessment) -> String {
    assessment
        .resume_rereads
        .map(|reread| match assessment.must_show_total {
            // The summary view's page offsets count its must_show rows,
            // which are listed, not counted.
            Some(_) => format!(
                "; it re-reads page {} (must_show rows {}-{}), already listed",
                reread.page, reread.first_row, reread.last_row
            ),
            None => format!(
                "; it re-reads page {} (rows {}-{}), already counted",
                reread.page, reread.first_row, reread.last_row
            ),
        })
        .unwrap_or_default()
}

/// Where the full history can be paged at the captured cut. Only the
/// tracker's own cursors bind that cut; the cursorless first-page command
/// reads the record's assessment as it stands now, so it is never offered as
/// a same-cut continuation.
fn acceptance_obligation_history_text(assessment: &AcceptanceObligationAssessment) -> String {
    if assessment.must_show_total.is_some() {
        // The summary view names its own history command at the cut.
        return match &assessment.history_continuation {
            Some(history) => format!(
                "the full history at this cut, oldest first, is `{history}` and then each page's continuation; the tracker refuses these cursors once the record's assessment moves on"
            ),
            None => format!(
                "No same-cut history command exists for this record; `{}` reads its current summary, not this cut",
                assessment.full_history
            ),
        };
    }
    match &assessment.history_continuation {
        Some(continuation) => format!(
            "the full history at this cut, oldest first, is page 1 by `{}` and then `{continuation}`; the tracker refuses these cursors once the record's assessment moves on",
            assessment.full_history
        ),
        None if assessment.complete => format!(
            "this one page is the whole history; `{}` reads the record's current state",
            assessment.full_history
        ),
        None => format!(
            "No same-cut continuation exists for this record; `{}` reads its current state, not this cut",
            assessment.full_history
        ),
    }
}

/// One record's summary as the brief carries it, whole.
fn render_acceptance_obligation_assessment(assessment: &AcceptanceObligationAssessment) -> String {
    let positions = match (assessment.record_position, assessment.cut_position) {
        (Some(record), Some(cut)) => format!(" (record position {record}, cut position {cut})"),
        (Some(record), None) => format!(" (record position {record})"),
        _ => String::new(),
    };
    let reason = assessment.incomplete_reason.as_deref().unwrap_or("unknown");
    let state = match (assessment.complete, assessment.must_show_total) {
        (true, None) => format!("{} rows, read whole.", acceptance_obligation_total(assessment)),
        (true, Some(must_show)) => format!(
            "{} rows, read whole: the {must_show} must_show rows are listed below, the rest only counted.",
            acceptance_obligation_total(assessment)
        ),
        (false, None) => format!(
            "INCOMPLETE: {} ({reason}). The counts and rows below cover only the rows read; an unread row may be open or a mismatch.",
            acceptance_obligation_read_extent(assessment)
        ),
        (false, Some(_)) => format!(
            "INCOMPLETE: {} ({reason}). The counts cover every row; an unread must_show row may be open or a mismatch.",
            acceptance_obligation_read_extent(assessment)
        ),
    };
    let groups = if assessment.groups.is_empty() {
        " No rows read.".to_owned()
    } else {
        format!(
            " {}.",
            assessment
                .groups
                .iter()
                .map(acceptance_obligation_group_label)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let mut lines = vec![format!(
        "Obligation assessment of {}{positions}: {state}{groups}",
        assessment.locator
    )];
    lines.extend(assessment.rows.iter().map(acceptance_obligation_row_line));
    if let Some(resume) = &assessment.resume {
        lines.push(format!(
            "  Resume at the same cut: `{resume}`{}.",
            acceptance_obligation_reread_text(assessment)
        ));
    }
    lines.push(match (&assessment.history_continuation, assessment.must_show_total) {
        (Some(continuation), None) => format!(
            "  Full history at this cut, oldest first: page 1 by `{}`, then `{continuation}`. The tracker refuses these cursors once the record's assessment moves on.",
            assessment.full_history
        ),
        (Some(history), Some(_)) => format!(
            "  Full history at this cut, oldest first: `{history}`, then each page's continuation. The tracker refuses these cursors once the record's assessment moves on."
        ),
        (None, _) => format!("  {}.", acceptance_obligation_history_text(assessment)),
    });
    lines.join("\n")
}

/// The line that names a summary the brief has no room for. It keeps what
/// the summary would have said of the read itself: whether it was whole, how
/// many rows were read, why it stopped and where to resume.
fn acceptance_obligation_not_carried_line(assessment: &AcceptanceObligationAssessment) -> String {
    let state = if assessment.complete {
        format!(
            "{}, read whole, {} to be listed in full",
            acceptance_obligation_whole_extent(assessment),
            assessment.rows.len()
        )
    } else {
        format!(
            "INCOMPLETE, {} ({}), {} of them to be listed in full; an unread {} may be open or a mismatch",
            acceptance_obligation_read_extent(assessment),
            assessment.incomplete_reason.as_deref().unwrap_or("unknown"),
            assessment.rows.len(),
            if assessment.must_show_total.is_some() { "must_show row" } else { "row" }
        )
    };
    let resume = assessment
        .resume
        .as_deref()
        .map(|resume| {
            format!(
                "resume at the same cut with `{resume}`{}; ",
                acceptance_obligation_reread_text(assessment)
            )
        })
        .unwrap_or_default();
    format!(
        "Obligation assessment of {}: not carried to fit the brief ({state}); {resume}{}.",
        assessment.locator,
        acceptance_obligation_history_text(assessment)
    )
}

/// The assessment section for a finished brief that leaves `room` bytes. A
/// summary is carried whole or named as not carried, in index order; it never
/// takes room from what the brief already holds. Marks what is carried.
fn acceptance_obligation_section(
    assessments: &mut [AcceptanceObligationAssessment],
    room: usize,
) -> String {
    const HEADER: &str = "Obligation assessments of the cited verification records (rows another record closed are counted, not listed, except mismatches, which are always listed; every other row is listed in full):\n";
    // The summary view decides which rows are listed: the tracker's must_show.
    const SUMMARY_HEADER: &str = "Obligation assessments of the cited verification records (rows another record closed are counted, not listed, except mismatches, which are always listed; every other row is listed in full; for a record in the tracker's summary view, its must_show rows are listed in full and every other row is only counted):\n";
    let header = if assessments.iter().any(|assessment| assessment.must_show_total.is_some()) {
        SUMMARY_HEADER
    } else {
        HEADER
    };
    let mut used = header.len() + 1;
    let mut body = String::new();
    for assessment in assessments.iter_mut() {
        let whole = format!("{}\n", render_acceptance_obligation_assessment(assessment));
        if used + whole.len() <= room {
            used += whole.len();
            body.push_str(&whole);
            assessment.carried = true;
            continue;
        }
        assessment.carried = false;
        let short = format!("{}\n", acceptance_obligation_not_carried_line(assessment));
        if used + short.len() <= room {
            used += short.len();
            body.push_str(&short);
        }
    }
    if body.is_empty() {
        String::new()
    } else {
        format!("{header}{body}\n")
    }
}

/// `prompt` with the assessment section placed before `anchor`, using only the
/// room left under `max_bytes`.
fn acceptance_prompt_with_obligations(
    prompt: String,
    anchor: &str,
    assessments: &mut [AcceptanceObligationAssessment],
    max_bytes: usize,
) -> String {
    if assessments.is_empty() {
        return prompt;
    }
    let Some(at) = acceptance_brief_anchor_offset(&prompt, anchor) else {
        return prompt;
    };
    let section = acceptance_obligation_section(
        assessments,
        max_bytes.saturating_sub(prompt.len()),
    );
    if section.is_empty() {
        return prompt;
    }
    format!("{}{section}{}", &prompt[..at], &prompt[at..])
}

/// Where the host's `anchor` line starts: the last line that starts with it,
/// since record text quoted earlier in the brief may contain the same phrase.
fn acceptance_brief_anchor_offset(prompt: &str, anchor: &str) -> Option<usize> {
    prompt
        .rfind(&format!("\n{anchor}"))
        .map(|newline| newline + 1)
        .or_else(|| prompt.starts_with(anchor).then_some(0))
}

/// The line that names, in the brief, the records whose assessment it does
/// not carry even as a one-line note, or none when every one is named.
fn acceptance_obligation_names_line(
    prompt: &str,
    assessments: &[AcceptanceObligationAssessment],
) -> Option<String> {
    let unnamed = assessments
        .iter()
        .filter(|assessment| {
            !assessment.carried
                && !prompt.contains(&acceptance_obligation_not_carried_line(assessment))
        })
        .map(|assessment| assessment.locator.as_str())
        .collect::<Vec<_>>();
    (!unnamed.is_empty()).then(|| {
        format!(
            "Obligation assessments not carried in this brief (the requester notice has them): {}.\n",
            unnamed.join(", ")
        )
    })
}

/// What an independent brief rendered at `plan` shows of its protected
/// content: the line of each protected window entry, or `None` where the
/// entry is left out, and the whole criterion evidence index. Protected are
/// every criterion-cited record and every holder entry in the window, cited
/// or not, since a holder's fail-first note cannot be told from the holder's
/// other notes. The lines are those the brief lists, from the same renderer.
fn acceptance_brief_protected_rendering(
    task: &AcceptanceEvaluationTask,
    plan: &AcceptanceBriefPlan,
) -> (Vec<Option<String>>, String) {
    let cited: BTreeSet<&str> = task
        .criterion_evidence
        .iter()
        .flat_map(|link| link.locators.iter().map(String::as_str))
        .collect();
    let start = task.evidence.len() - plan.shown;
    let mut fully_rendered = BTreeSet::new();
    let mut lines = Vec::new();
    for (index, entry) in task.evidence.iter().enumerate() {
        let line = (index >= start).then(|| {
            let (line, clipped) = acceptance_brief_evidence_line(entry, index - start < plan.clipped);
            if !clipped && !entry.cut_by_tracker && entry.summary.is_some() {
                fully_rendered.insert(entry.locator.clone());
            }
            line
        });
        if !entry.non_holder || cited.contains(entry.locator.as_str()) {
            lines.push(line);
        }
    }
    let index = render_acceptance_criterion_evidence(task, plan.index_detail, &fully_rendered, false);
    (lines, index)
}


/// Fits the summaries into `prompt`, then names before the anchor any record
/// still not named, if the result fits `max_bytes`.
fn acceptance_prompt_naming_obligations(
    prompt: String,
    anchor: &str,
    assessments: &mut [AcceptanceObligationAssessment],
    max_bytes: usize,
    reserve: usize,
) -> Option<String> {
    let fitted = acceptance_prompt_with_obligations(
        prompt,
        anchor,
        assessments,
        max_bytes.saturating_sub(reserve),
    );
    let named = match acceptance_obligation_names_line(&fitted, assessments) {
        None => fitted,
        Some(line) => {
            let at = acceptance_brief_anchor_offset(&fitted, anchor)?;
            format!("{}{line}{}", &fitted[..at], &fitted[at..])
        }
    };
    (named.len() <= max_bytes).then_some(named)
}

/// The independent brief with its assessment summaries. When a record's
/// assessment cannot be named in the room `base` leaves, the brief is rendered
/// again from `base`'s own plan, changing only the omission detail and the
/// window (clipped, then left out, oldest first), never the criterion evidence
/// index, the carried failure or the outcome. A rebuild is tried only when it
/// shows its protected content byte for byte as `base` does
/// ([`acceptance_brief_protected_rendering`]): each protected entry with the
/// same line, whole or clipped, never newly left out, and the same criterion
/// evidence index. So only the omission detail and the window entries of
/// non-holders the requester did not cite give way; otherwise `base` stays as
/// it is and the requester notice alone names the record.
fn acceptance_independent_brief_with_obligations(
    task: &AcceptanceEvaluationTask,
    cwd: &str,
    command_form: &str,
    base: AcceptanceEvaluatorBrief,
    assessments: &mut [AcceptanceObligationAssessment],
    max_bytes: usize,
) -> AcceptanceEvaluatorBrief {
    let fitted = acceptance_prompt_with_obligations(
        base.prompt.clone(),
        ACCEPTANCE_ASSESSMENT_ANCHOR,
        assessments,
        max_bytes,
    );
    let Some(reserve) = acceptance_obligation_names_line(&fitted, assessments).map(|line| line.len())
    else {
        return AcceptanceEvaluatorBrief { prompt: fitted, ..base };
    };
    let plan = base.plan;
    let protected = acceptance_brief_protected_rendering(task, &plan);
    let details = [
        AcceptanceOmissionDetail::Full,
        AcceptanceOmissionDetail::Compact,
        AcceptanceOmissionDetail::Minimal,
    ]
    .into_iter()
    .skip_while(move |detail| *detail != plan.detail);
    let windows = (plan.clipped..=plan.shown)
        .map(move |clipped| (plan.shown, clipped))
        .chain((0..plan.shown).rev().map(|shown| (shown, shown)));
    let candidates = details.flat_map(move |detail| {
        windows.clone().map(move |(shown, clipped)| AcceptanceBriefPlan {
            shown,
            clipped,
            detail,
            ..plan
        })
    });
    for candidate in candidates.filter(|candidate| *candidate != plan) {
        if acceptance_brief_protected_rendering(task, &candidate) != protected {
            continue;
        }
        let rebuilt = render_acceptance_evaluator_brief_with_index_detail(
            task,
            cwd,
            command_form,
            candidate.shown,
            candidate.clipped,
            candidate.outcome_bytes,
            candidate.detail,
            candidate.carried,
            candidate.index_detail,
        );
        if rebuilt.prompt.len() + reserve > max_bytes {
            continue;
        }
        let mut trial = assessments.to_vec();
        if let Some(prompt) = acceptance_prompt_naming_obligations(
            rebuilt.prompt.clone(),
            ACCEPTANCE_ASSESSMENT_ANCHOR,
            &mut trial,
            max_bytes,
            reserve,
        ) {
            assessments.clone_from_slice(&trial);
            return AcceptanceEvaluatorBrief { prompt, ..rebuilt };
        }
    }
    AcceptanceEvaluatorBrief { prompt: fitted, ..base }
}

/// The same-session brief with its assessment summaries. The same rule as
/// [`acceptance_independent_brief_with_obligations`]: when a record cannot be
/// named in the room left, the brief is rendered again at a lower omission
/// detail, keeping its index detail, and kept only when the earlier checks it
/// lists and its carried failure render exactly as before, so that only the
/// omission wording gives way; otherwise the notice alone names it.
fn acceptance_same_session_brief_with_obligations(
    task: &AcceptanceEvaluationTask,
    source_fingerprint: Option<&str>,
    base: (String, AcceptanceBriefCuts, AcceptanceOmissionDetail, AcceptanceOmissionDetail),
    assessments: &mut [AcceptanceObligationAssessment],
    max_bytes: usize,
) -> (String, AcceptanceBriefCuts) {
    let (prompt, cuts, detail, index_detail) = base;
    let fitted = acceptance_prompt_with_obligations(
        prompt,
        ACCEPTANCE_SAME_SESSION_ASSESSMENT_ANCHOR,
        assessments,
        max_bytes,
    );
    let Some(reserve) = acceptance_obligation_names_line(&fitted, assessments).map(|line| line.len())
    else {
        return (fitted, cuts);
    };
    for lower in [AcceptanceOmissionDetail::Compact, AcceptanceOmissionDetail::Minimal]
        .into_iter()
        .filter(|lower| match detail {
            AcceptanceOmissionDetail::Full => true,
            AcceptanceOmissionDetail::Compact => *lower == AcceptanceOmissionDetail::Minimal,
            AcceptanceOmissionDetail::Minimal => false,
        })
    {
        let rebuilt = render_same_session_acceptance_brief_with_index_detail(
            task,
            source_fingerprint,
            lower,
            index_detail,
        );
        // A same-session brief lists no window bodies, and its criterion
        // evidence index and cuts depend on the index detail alone, which the
        // rebuild keeps: they are the same as before. The omission detail also
        // selects how many earlier checks are listed and how much of a carried
        // failure is shown; neither may give way, so only the omission wording
        // changes.
        let keeps_records = acceptance_same_session_verifications(task, lower)
            == acceptance_same_session_verifications(task, detail)
            && acceptance_brief_carried_failure(task, lower, true)
                == acceptance_brief_carried_failure(task, detail, true);
        if rebuilt.len() + reserve > max_bytes || !keeps_records {
            continue;
        }
        let mut trial = assessments.to_vec();
        if let Some(prompt) = acceptance_prompt_naming_obligations(
            rebuilt,
            ACCEPTANCE_SAME_SESSION_ASSESSMENT_ANCHOR,
            &mut trial,
            max_bytes,
            reserve,
        ) {
            assessments.clone_from_slice(&trial);
            return (prompt, cuts);
        }
    }
    (fitted, cuts)
}

/// One sentence for the requester about assessments the evaluator does not
/// get whole, or none.
fn acceptance_obligation_assessment_notice(
    assessments: &[AcceptanceObligationAssessment],
) -> Option<String> {
    let parts = assessments
        .iter()
        .filter(|assessment| !assessment.complete || !assessment.carried)
        .map(|assessment| {
            let carried = if assessment.carried {
                ""
            } else {
                ", and the brief does not carry it"
            };
            let history = acceptance_obligation_history_text(assessment);
            if assessment.complete {
                format!(
                    "{}: read whole ({}) but not carried to fit the brief; {history}",
                    assessment.locator,
                    acceptance_obligation_whole_extent(assessment),
                )
            } else {
                format!(
                    "{}: incomplete, {} ({}){carried}{}; {history}",
                    assessment.locator,
                    acceptance_obligation_read_extent(assessment),
                    assessment.incomplete_reason.as_deref().unwrap_or("unknown"),
                    assessment
                        .resume
                        .as_deref()
                        .map(|resume| format!(
                            "; resume at the same cut with `{resume}`{}",
                            acceptance_obligation_reread_text(assessment)
                        ))
                        .unwrap_or_default()
                )
            }
        })
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| {
        format!(
            "Obligation assessments the evaluator does not get whole: {}. A row not read may be open or a mismatch.",
            parts.join("; ")
        )
    })
}
