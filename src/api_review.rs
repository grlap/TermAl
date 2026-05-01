// Code review HTTP routes.
//
// TermAl's optional "review" feature lets an agent produce a
// structured review document (threaded comments on specific file
// ranges) which the frontend renders in a dedicated review pane.
// Three handlers:
//
// - `get_review` — returns the review document for a given
//   `change_set_id` (typically a commit sha or branch diff
//   identifier).
// - `put_review` — stores or updates the review document. Used by
//   agents that produce reviews + by manual user edits in the UI.
// - `get_review_summary` — returns a lightweight summary (thread
//   count, has-unresolved flag) for the sidebar badge without
//   fetching the full document.
//
// Storage lives under `<workdir>/.termal/reviews/…` and is managed
// by the functions in `src/review.rs`. Each handler forwards to the
// review backend via `run_blocking_api` so disk I/O doesn't block
// the async runtime.


/// Gets review.
async fn get_review(
    AxumPath(change_set_id): AxumPath<String>,
    Query(query): Query<ReviewQuery>,
    State(state): State<AppState>,
) -> Result<Json<ReviewDocumentResponse>, ApiError> {
    validate_review_change_set_id(&change_set_id)?;
    let response = run_blocking_api(move || {
        if let Some(scope) = state
            .remote_scope_for_request(query.session_id.as_deref(), query.project_id.as_deref())?
        {
            return state.remote_get_json(
                &scope,
                &format!("/api/reviews/{}", encode_uri_component(&change_set_id)),
                Vec::new(),
            );
        }

        let review_root = resolve_review_storage_root(
            &state,
            query.session_id.as_deref(),
            query.project_id.as_deref(),
        )?;
        let review_path = resolve_review_document_path(&review_root, &change_set_id)?;
        let review = {
            let _review_guard = state
                .review_documents_lock
                .lock()
                .expect("review documents mutex poisoned");
            load_review_document(&review_path, &change_set_id)?
        };
        Ok(ReviewDocumentResponse {
            review_file_path: review_path.to_string_lossy().into_owned(),
            review,
        })
    })
    .await?;
    Ok(Json(response))
}

/// Stores review.
async fn put_review(
    AxumPath(change_set_id): AxumPath<String>,
    Query(query): Query<ReviewQuery>,
    State(state): State<AppState>,
    Json(review): Json<ReviewDocument>,
) -> Result<Json<ReviewDocumentResponse>, ApiError> {
    validate_review_change_set_id(&change_set_id)?;
    let response = run_blocking_api(move || {
        state.ensure_read_only_delegation_allows_session_write_action(
            query.session_id.as_deref(),
            "review document writes",
        )?;
        if let Some(scope) = state
            .remote_scope_for_request(query.session_id.as_deref(), query.project_id.as_deref())?
        {
            return state.remote_put_json_with_query_scope(
                &scope,
                &format!("/api/reviews/{}", encode_uri_component(&change_set_id)),
                Vec::new(),
                serde_json::to_value(&review).map_err(|err| {
                    ApiError::internal(format!("failed to encode review payload: {err}"))
                })?,
            );
        }

        state.ensure_read_only_delegation_allows_write_action(
            query.session_id.as_deref(),
            query.project_id.as_deref(),
            None,
            "review document writes",
        )?;

        let review_root = resolve_review_storage_root(
            &state,
            query.session_id.as_deref(),
            query.project_id.as_deref(),
        )?;
        let review_path = resolve_review_document_path(&review_root, &change_set_id)?;
        let persisted_review = {
            let _review_guard = state
                .review_documents_lock
                .lock()
                .expect("review documents mutex poisoned");
            let persisted =
                prepare_review_document_for_write(&review_path, &change_set_id, review)?;
            persist_review_document(&review_path, &persisted)?;
            persisted
        };
        Ok(ReviewDocumentResponse {
            review_file_path: review_path.to_string_lossy().into_owned(),
            review: persisted_review,
        })
    })
    .await?;
    Ok(Json(response))
}

/// Gets review summary.
async fn get_review_summary(
    AxumPath(change_set_id): AxumPath<String>,
    Query(query): Query<ReviewQuery>,
    State(state): State<AppState>,
) -> Result<Json<ReviewSummaryResponse>, ApiError> {
    validate_review_change_set_id(&change_set_id)?;
    let response = run_blocking_api(move || {
        if let Some(scope) = state
            .remote_scope_for_request(query.session_id.as_deref(), query.project_id.as_deref())?
        {
            return state.remote_get_json(
                &scope,
                &format!(
                    "/api/reviews/{}/summary",
                    encode_uri_component(&change_set_id)
                ),
                Vec::new(),
            );
        }

        let review_root = resolve_review_storage_root(
            &state,
            query.session_id.as_deref(),
            query.project_id.as_deref(),
        )?;
        let review_path = resolve_review_document_path(&review_root, &change_set_id)?;
        let review = {
            let _review_guard = state
                .review_documents_lock
                .lock()
                .expect("review documents mutex poisoned");
            load_review_document(&review_path, &change_set_id)?
        };
        let summary = summarize_review_document(&review);

        Ok(ReviewSummaryResponse {
            change_set_id: review.change_set_id,
            review_file_path: review_path.to_string_lossy().into_owned(),
            thread_count: summary.thread_count,
            open_thread_count: summary.open_thread_count,
            resolved_thread_count: summary.resolved_thread_count,
            comment_count: summary.comment_count,
            has_threads: summary.thread_count > 0,
        })
    })
    .await?;
    Ok(Json(response))
}
