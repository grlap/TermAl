// New Work visualizer wire types and Engram receipt normalization.
// Owns read models and strict required-field validation, not CLI execution,
// host reader selection, work mutation, or HTML rendering.

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkListQuery {
    search: Option<String>,
    label: Option<String>,
    availability: Option<String>,
    after: Option<String>,
    reader_id: Option<String>,
}

impl WorkListQuery {
    fn validate(&self) -> Result<(), ApiError> {
        for value in [&self.search, &self.label, &self.after, &self.reader_id]
            .into_iter()
            .flatten()
        {
            if value.len() > 16 * 1024 || value.chars().any(char::is_control) {
                return Err(ApiError::bad_request(
                    "Work query contains oversized or control text",
                ));
            }
        }
        if self
            .availability
            .as_deref()
            .is_some_and(|v| !matches!(v, "ready" | "blocked"))
        {
            return Err(ApiError::bad_request(
                "Work availability filter must be ready or blocked",
            ));
        }
        if self.after.is_some() && self.reader_id.is_none() {
            return Err(ApiError::bad_request(
                "Work continuation requires its readerId",
            ));
        }
        Ok(())
    }

    fn arguments(&self) -> Vec<String> {
        // Open work only, like the Beads snapshot: `--all` would add every
        // completed, cancelled and superseded item to the default view.
        let mut args = vec!["ls", "--verbose", "--json", "--limit", "20"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        for (flag, value) in [
            ("--search", &self.search),
            ("--label", &self.label),
            ("--after", &self.after),
        ] {
            if let Some(value) = value {
                args.push(format!("{flag}={value}"));
            }
        }
        if let Some(availability) = &self.availability {
            args.push(format!("--{availability}"));
        }
        args
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkSourceStatus {
    source: &'static str,
    state: &'static str,
    message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkListResponse {
    sources: Vec<WorkSourceStatus>,
    reader_id: Option<String>,
    observed_at: String,
    page: Option<WorkPage>,
    /// Beads rows share the item model but never the Engram reader/cursor
    /// contract: one full read, no continuation, explicit per-source errors.
    beads: Option<WorkPage>,
}

/// A blocking relation ("waits for"), distinct from parent/child hierarchy.
/// `satisfied` marks a prerequisite already closed/completed.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct WorkPrerequisiteView {
    id: String,
    satisfied: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkItemView {
    source: String,
    id: String,
    short_ref: String,
    title: String,
    kind: String,
    lifecycle: String,
    availability: String,
    priority: u8,
    labels: Vec<String>,
    assigned_to: Option<String>,
    parent_id: Option<String>,
    updated_at: String,
    blocked_by: Vec<String>,
    prerequisites: Vec<WorkPrerequisiteView>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkPage {
    items: Vec<WorkItemView>,
    total: usize,
    shown_before: usize,
    more: bool,
    after: Option<String>,
    hint: Option<String>,
}

#[derive(Deserialize)]
struct EngramWorkListReceipt {
    items: Vec<EngramWorkListRow>,
    total: usize,
    shown_before: usize,
    more: bool,
    after: Option<String>,
    hint: Option<String>,
}

#[derive(Deserialize)]
struct EngramWorkListRow {
    work: EngramWorkListItem,
    availability: String,
    blocked_by: Vec<String>,
}

#[derive(Deserialize)]
struct EngramWorkListItem {
    work_id: String,
    short_ref: String,
    title: String,
    kind: String,
    lifecycle: String,
    priority: u8,
    labels: Vec<String>,
    assigned_to: Option<String>,
    parent_id: Option<String>,
    updated_at: String,
}

fn normalize_engram_work_page(value: Value) -> Result<WorkPage, ApiError> {
    let receipt: EngramWorkListReceipt = serde_json::from_value(value)
        .map_err(|e| ApiError::bad_gateway(format!("engram work ls: invalid receipt: {e}")))?;
    let mut ids = HashSet::new();
    let end = receipt.shown_before.checked_add(receipt.items.len());
    if receipt.items.iter().any(|row| {
        row.work.priority > 4
            || row.work.work_id.is_empty()
            || row.work.short_ref.is_empty()
            || !ids.insert(&row.work.work_id)
    }) || end.is_none_or(|end| end > receipt.total || receipt.more != (end < receipt.total))
        || receipt
            .after
            .as_ref()
            .is_some_and(|after| after.is_empty() || !receipt.more || receipt.items.is_empty())
    {
        return Err(ApiError::bad_gateway(
            "engram work ls: invalid item or page counts",
        ));
    }
    Ok(WorkPage {
        items: receipt
            .items
            .into_iter()
            .map(|row| WorkItemView {
                source: "engram".to_owned(),
                id: row.work.work_id,
                short_ref: row.work.short_ref,
                title: row.work.title,
                kind: row.work.kind,
                lifecycle: row.work.lifecycle,
                availability: row.availability,
                priority: row.work.priority,
                labels: row.work.labels,
                assigned_to: row.work.assigned_to,
                parent_id: row.work.parent_id,
                updated_at: row.work.updated_at,
                // Engram lists only the still-blocking prerequisites; the
                // satisfied ones are visible through `show`, not here.
                prerequisites: row
                    .blocked_by
                    .iter()
                    .map(|id| WorkPrerequisiteView {
                        id: id.clone(),
                        satisfied: false,
                    })
                    .collect(),
                blocked_by: row.blocked_by,
            })
            .collect(),
        total: receipt.total,
        shown_before: receipt.shown_before,
        more: receipt.more,
        after: receipt.after,
        hint: receipt.hint,
    })
}
