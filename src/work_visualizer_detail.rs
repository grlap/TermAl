// New Work item detail/window boundary. Normalizes the documented Engram
// show surface; does not execute navigation suggestions or fetch full bodies
// implicitly. Continuation headers deliberately do not replace item detail.

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkDetailQuery {
    reader_session_id: String,
    after: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkDetailFields {
    #[serde(alias = "short_ref")]
    short_ref: String,
    title: String,
    outcome: String,
    acceptance: Vec<String>,
    #[serde(alias = "acceptance_omitted")]
    acceptance_omitted: Option<usize>,
    lifecycle: String,
    kind: String,
    priority: u8,
}

#[derive(Debug, Deserialize, Serialize)]
struct WorkDetailStatus {
    work: WorkDetailFields,
    availability: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkNoteView {
    locator: String,
    kind: String,
    family: String,
    #[serde(alias = "status_owner")]
    status_owner: Option<bool>,
    #[serde(alias = "non_holder")]
    non_holder: Option<bool>,
    // Omitted bodies may also omit refs. None is not an empty reference list.
    refs: Option<Vec<String>>,
    summary: Option<String>,
    by: Option<String>,
    #[serde(alias = "created_at")]
    created_at: String,
    #[serde(default, alias = "body_omitted")]
    body_omitted: bool,
    #[serde(default, alias = "summary_truncated")]
    summary_truncated: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkNotesWindow {
    total: usize,
    shown: usize,
    newer: usize,
    older: usize,
    after: Option<String>,
    #[serde(alias = "read_cut")]
    read_cut: WorkNotesCut,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct WorkNotesCut {
    #[serde(alias = "project_position")]
    project_position: u64,
    #[serde(alias = "observed_at")]
    observed_at: String,
    #[serde(alias = "valid_until_ms")]
    valid_until_ms: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkDetailResponse {
    status: Option<WorkDetailStatus>,
    holder: Option<String>,
    #[serde(alias = "held_until")]
    held_until: Option<String>,
    notes: Vec<WorkNoteView>,
    #[serde(alias = "notes_window")]
    notes_window: WorkNotesWindow,
}

impl AppState {
    #[cfg(test)]
    fn read_project_work_detail(
        &self,
        project_id: &str,
        work_ref: &str,
        query: WorkDetailQuery,
    ) -> Result<WorkDetailResponse, ApiError> {
        self.read_project_work_detail_with_admission(project_id, work_ref, query, || Ok(()))
    }

    fn read_project_work_detail_with_admission<P>(
        &self,
        project_id: &str,
        work_ref: &str,
        query: WorkDetailQuery,
        admit: impl FnOnce() -> Result<P, ApiError>,
    ) -> Result<WorkDetailResponse, ApiError> {
        WorkListQuery {
            reader_session_id: Some(query.reader_session_id.clone()),
            after: query.after.clone(),
            ..Default::default()
        }
        .validate()?;
        if work_ref.is_empty()
            || work_ref.len() > 256
            || work_ref.starts_with('-')
            || work_ref.chars().any(char::is_control)
        {
            return Err(ApiError::bad_request("Invalid Work reference"));
        }
        let (_, target) = self.work_read_snapshot(project_id, Some(&query.reader_session_id))?;
        let target = target.ok_or_else(|| {
            ApiError::conflict("Established Work reader unavailable; refresh the list")
        })?;
        validate_work_read_target(&target)?;
        let mut args = vec![
            "show".to_owned(),
            work_ref.to_owned(),
            "--notes".to_owned(),
            "--gates".to_owned(),
            "--json".to_owned(),
        ];
        if let Some(after) = &query.after {
            args.push(format!("--after={after}"));
        }
        let _permit = admit()?;
        self.validate_work_read_still_current(project_id, &target)?;
        let value = run_work_read_command(&target.connection, &args)?;
        self.validate_work_read_still_current(project_id, &target)?;
        normalize_engram_work_detail(value, query.after.is_some())
    }
}

fn normalize_engram_work_detail(
    value: Value,
    continuation: bool,
) -> Result<WorkDetailResponse, ApiError> {
    let detail: WorkDetailResponse = serde_json::from_value(value)
        .map_err(|e| ApiError::bad_gateway(format!("engram work show: invalid receipt: {e}")))?;
    let window = &detail.notes_window;
    if (!continuation && detail.status.is_none())
        || detail.status.as_ref().is_some_and(|s| s.work.priority > 4)
        || detail.notes.len() != window.shown
        || window
            .newer
            .saturating_add(window.shown)
            .saturating_add(window.older)
            != window.total
    {
        return Err(ApiError::bad_gateway(
            "engram work show: invalid detail/window counts",
        ));
    }
    Ok(detail)
}

async fn get_project_work_detail(
    State(state): State<AppState>,
    AxumPath((project_id, work_ref)): AxumPath<(String, String)>,
    limiter: Option<axum::Extension<WorkReadLimiter>>,
    query: Result<Query<WorkDetailQuery>, QueryRejection>,
) -> Result<Json<WorkDetailResponse>, ApiError> {
    let Query(query) = query.map_err(|e| api_query_rejection("work detail", e))?;
    let limiter = limiter.map_or_else(|| WORK_READ_PERMITS.clone(), |limiter| limiter.0.0);
    let runtime = tokio::runtime::Handle::current();
    run_blocking_api(move || {
        state
            .read_project_work_detail_with_admission(&project_id, &work_ref, query, || {
                runtime.block_on(acquire_work_read_permit_from(limiter))
            })
            .map(Json)
    })
    .await
}
