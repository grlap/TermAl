// New Work item detail/window boundary. Normalizes the documented Engram
// show surface; does not execute navigation suggestions or fetch full bodies
// implicitly. Continuation headers deliberately do not replace item detail.

use super::*;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WorkDetailQuery {
    pub(crate) reader_id: String,
    pub(crate) after: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkDetailFields {
    #[serde(alias = "short_ref")]
    pub(crate) short_ref: String,
    pub(crate) title: String,
    pub(crate) outcome: String,
    pub(crate) acceptance: Vec<String>,
    #[serde(alias = "acceptance_omitted")]
    pub(crate) acceptance_omitted: Option<usize>,
    pub(crate) lifecycle: String,
    pub(crate) kind: String,
    pub(crate) priority: u8,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct WorkDetailStatus {
    pub(crate) work: WorkDetailFields,
    pub(crate) availability: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkNoteView {
    pub(crate) locator: String,
    pub(crate) kind: String,
    pub(crate) family: String,
    #[serde(alias = "status_owner")]
    pub(crate) status_owner: Option<bool>,
    #[serde(alias = "non_holder")]
    pub(crate) non_holder: Option<bool>,
    // Omitted bodies may also omit refs. None is not an empty reference list.
    pub(crate) refs: Option<Vec<String>>,
    pub(crate) summary: Option<String>,
    pub(crate) by: Option<String>,
    #[serde(alias = "created_at")]
    pub(crate) created_at: String,
    #[serde(default, alias = "body_omitted")]
    pub(crate) body_omitted: bool,
    #[serde(default, alias = "summary_truncated")]
    pub(crate) summary_truncated: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkNotesWindow {
    pub(crate) total: usize,
    pub(crate) shown: usize,
    pub(crate) newer: usize,
    pub(crate) older: usize,
    pub(crate) after: Option<String>,
    #[serde(alias = "read_cut")]
    pub(crate) read_cut: WorkNotesCut,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkNotesCut {
    #[serde(alias = "project_position")]
    pub(crate) project_position: u64,
    #[serde(alias = "observed_at")]
    pub(crate) observed_at: String,
    #[serde(alias = "valid_until_ms")]
    pub(crate) valid_until_ms: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkDetailResponse {
    pub(crate) status: Option<WorkDetailStatus>,
    pub(crate) holder: Option<String>,
    #[serde(alias = "held_until")]
    pub(crate) held_until: Option<String>,
    pub(crate) notes: Vec<WorkNoteView>,
    #[serde(alias = "notes_window")]
    pub(crate) notes_window: WorkNotesWindow,
}

impl AppState {
    #[cfg(test)]
    pub(crate) fn read_project_work_detail(
        &self,
        project_id: &str,
        work_ref: &str,
        query: WorkDetailQuery,
    ) -> Result<WorkDetailResponse, ApiError> {
        self.read_project_work_detail_with_admission(project_id, work_ref, query, || Ok(()))
    }

    pub(crate) fn read_project_work_detail_with_admission<P>(
        &self,
        project_id: &str,
        work_ref: &str,
        query: WorkDetailQuery,
        admit: impl FnOnce() -> Result<P, ApiError>,
    ) -> Result<WorkDetailResponse, ApiError> {
        WorkListQuery {
            reader_id: Some(query.reader_id.clone()),
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
        let (_, target) = self.work_read_snapshot(project_id, Some(&query.reader_id))?;
        let target = target.map_err(|reason| {
            ApiError::conflict(format!(
                "Work reader unavailable; refresh the list: {reason}"
            ))
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
        let value = run_work_read_command(
            &target.connection,
            &args,
            &self.engram_host_launch_workdir(),
        )?;
        self.validate_work_read_still_current(project_id, &target)?;
        normalize_engram_work_detail(value, query.after.is_some())
    }
}

pub(crate) fn normalize_engram_work_detail(
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

pub(crate) async fn get_project_work_detail(
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
