// Review-document storage — path resolution, load + persist with directory
// fsync, platform-specific atomic replace, serialization validation for
// threads/anchors/change-set ids, and the default empty document.
//
// HTTP handlers (`get_review`, `put_review`, `get_review_summary`) stay in
// api.rs. This file owns the on-disk review document lifecycle; everything
// is visible across the flat `include!()`-assembled module.

/// Resolves review storage root.
fn resolve_review_storage_root(
    state: &AppState,
    session_id: Option<&str>,
    project_id: Option<&str>,
) -> Result<PathBuf, ApiError> {
    resolve_request_project_root_path(state, session_id, project_id)
}

/// Resolves review document path.
fn resolve_review_document_path(
    review_root: &FsPath,
    change_set_id: &str,
) -> Result<PathBuf, ApiError> {
    validate_review_change_set_id(change_set_id)?;
    Ok(review_root
        .join(".termal")
        .join("reviews")
        .join(format!("{change_set_id}.json")))
}

/// Loads review document.
fn load_review_document(path: &FsPath, change_set_id: &str) -> Result<ReviewDocument, ApiError> {
    if !path.exists() {
        return Ok(default_review_document(change_set_id));
    }

    let raw = fs::read(path).map_err(|err| {
        ApiError::internal(format!(
            "failed to read review file {}: {err}",
            path.display()
        ))
    })?;
    let review: ReviewDocument = serde_json::from_slice(&raw).map_err(|err| {
        ApiError::internal(format!(
            "failed to parse review file {}: {err}",
            path.display()
        ))
    })?;
    validate_review_document(change_set_id, &review)?;
    Ok(review)
}

/// Persists review document.
fn persist_review_document(path: &FsPath, review: &ReviewDocument) -> Result<(), ApiError> {
    persist_review_document_with_directory_sync(path, review, sync_review_document_directory)
}

fn persist_review_document_with_directory_sync<F>(
    path: &FsPath,
    review: &ReviewDocument,
    sync_directory: F,
) -> Result<(), ApiError>
where
    F: FnOnce(&FsPath) -> Result<(), ApiError>,
{
    let parent = path.parent().ok_or_else(|| {
        ApiError::internal(format!(
            "review file {} has no parent directory",
            path.display()
        ))
    })?;
    fs::create_dir_all(parent).map_err(|err| {
        ApiError::internal(format!(
            "failed to create review directory {}: {err}",
            parent.display()
        ))
    })?;

    let encoded = serde_json::to_vec_pretty(review)
        .map_err(|err| ApiError::internal(format!("failed to serialize review document: {err}")))?;
    let file_name = path.file_name().ok_or_else(|| {
        ApiError::internal(format!(
            "review file {} is missing a file name",
            path.display()
        ))
    })?;
    let temp_path = parent.join(format!(
        ".{}.{}.tmp",
        file_name.to_string_lossy(),
        Uuid::new_v4()
    ));

    let write_result = (|| {
        let mut temp_file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to create temp review file {}: {err}",
                    temp_path.display()
                ))
            })?;
        temp_file.write_all(&encoded).map_err(|err| {
            ApiError::internal(format!(
                "failed to write temp review file {}: {err}",
                temp_path.display()
            ))
        })?;
        temp_file.sync_all().map_err(|err| {
            ApiError::internal(format!(
                "failed to flush temp review file {}: {err}",
                temp_path.display()
            ))
        })?;
        drop(temp_file);

        replace_review_document_file(&temp_path, path)?;
        Ok(())
    })();

    if let Err(err) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(err);
    }

    if let Err(err) = sync_directory(parent) {
        eprintln!(
            "review warning> review file {} replaced but parent directory sync failed: {}",
            path.display(),
            err.message
        );
    }

    Ok(())
}

#[cfg(windows)]
const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;

#[cfg(windows)]
const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

#[cfg(windows)]
#[link(name = "Kernel32")]
unsafe extern "system" {
    fn MoveFileExW(existing_file_name: *const u16, new_file_name: *const u16, flags: u32) -> i32;
}

#[cfg(not(windows))]
fn replace_review_document_file(temp_path: &FsPath, path: &FsPath) -> Result<(), ApiError> {
    fs::rename(temp_path, path).map_err(|err| {
        ApiError::internal(format!(
            "failed to replace review file {}: {err}",
            path.display()
        ))
    })
}

#[cfg(windows)]
fn replace_review_document_file(temp_path: &FsPath, path: &FsPath) -> Result<(), ApiError> {
    use std::os::windows::ffi::OsStrExt as _;

    let temp_path_wide = temp_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let path_wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();

    let moved = unsafe {
        MoveFileExW(
            temp_path_wide.as_ptr(),
            path_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(ApiError::internal(format!(
            "failed to replace review file {}: {}",
            path.display(),
            io::Error::last_os_error()
        )));
    }

    Ok(())
}

#[cfg(not(windows))]
fn sync_review_document_directory(path: &FsPath) -> Result<(), ApiError> {
    let directory = fs::File::open(path).map_err(|err| {
        ApiError::internal(format!(
            "failed to open review directory {} for sync: {err}",
            path.display()
        ))
    })?;
    directory.sync_all().map_err(|err| {
        ApiError::internal(format!(
            "failed to flush review directory {}: {err}",
            path.display()
        ))
    })
}

#[cfg(windows)]
fn sync_review_document_directory(_path: &FsPath) -> Result<(), ApiError> {
    Ok(())
}

/// Prepares review document for write.
fn prepare_review_document_for_write(
    path: &FsPath,
    change_set_id: &str,
    review: ReviewDocument,
) -> Result<ReviewDocument, ApiError> {
    validate_review_document(change_set_id, &review)?;
    let current = load_review_document(path, change_set_id)?;
    if review.revision != current.revision {
        return Err(ApiError::conflict(format!(
            "review document is out of date: expected revision {}, got {}",
            current.revision, review.revision
        )));
    }

    let next_revision = current
        .revision
        .checked_add(1)
        .ok_or_else(|| ApiError::internal("review revision overflow"))?;
    let mut next = review;
    next.revision = next_revision;
    Ok(next)
}

/// Returns the default review document.
fn default_review_document(change_set_id: &str) -> ReviewDocument {
    ReviewDocument {
        version: REVIEW_DOCUMENT_VERSION,
        revision: 0,
        change_set_id: change_set_id.to_owned(),
        origin: None,
        files: Vec::new(),
        threads: Vec::new(),
    }
}

/// Summarizes review document.
fn summarize_review_document(review: &ReviewDocument) -> ReviewDocumentSummary {
    let mut summary = ReviewDocumentSummary::default();
    summary.thread_count = review.threads.len();

    for thread in &review.threads {
        summary.comment_count += thread.comments.len();
        match thread.status {
            ReviewThreadStatus::Open => summary.open_thread_count += 1,
            ReviewThreadStatus::Resolved => summary.resolved_thread_count += 1,
            ReviewThreadStatus::Applied | ReviewThreadStatus::Dismissed => {}
        }
    }

    summary
}

const MAX_REVIEW_CHANGE_SET_ID_LEN: usize = 200;

/// Validates review change set ID.
fn validate_review_change_set_id(change_set_id: &str) -> Result<(), ApiError> {
    let trimmed = change_set_id.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request("changeSetId cannot be empty"));
    }

    if trimmed != change_set_id {
        return Err(ApiError::bad_request(
            "changeSetId may not have leading or trailing whitespace",
        ));
    }

    if change_set_id.len() > MAX_REVIEW_CHANGE_SET_ID_LEN {
        return Err(ApiError::bad_request(format!(
            "changeSetId is too long (max {MAX_REVIEW_CHANGE_SET_ID_LEN} bytes)"
        )));
    }

    if change_set_id.chars().all(|character| character == '.') {
        return Err(ApiError::bad_request(
            "changeSetId must not consist entirely of dots",
        ));
    }

    if change_set_id
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        return Ok(());
    }

    Err(ApiError::bad_request(
        "changeSetId may only contain letters, numbers, '.', '-', and '_'",
    ))
}

/// Validates review document.
fn validate_review_document(change_set_id: &str, review: &ReviewDocument) -> Result<(), ApiError> {
    if review.version != REVIEW_DOCUMENT_VERSION {
        return Err(ApiError::bad_request(format!(
            "unsupported review document version {}",
            review.version
        )));
    }

    if review.change_set_id != change_set_id {
        return Err(ApiError::bad_request(format!(
            "review changeSetId `{}` does not match route `{change_set_id}`",
            review.change_set_id
        )));
    }

    if let Some(origin) = review.origin.as_ref() {
        validate_non_empty_review_field("origin.sessionId", &origin.session_id)?;
        validate_non_empty_review_field("origin.messageId", &origin.message_id)?;
        validate_non_empty_review_field("origin.agent", &origin.agent)?;
        validate_non_empty_review_field("origin.workdir", &origin.workdir)?;
        validate_non_empty_review_field("origin.createdAt", &origin.created_at)?;
    }

    for file in &review.files {
        validate_non_empty_review_field("files[].filePath", &file.file_path)?;
    }

    for thread in &review.threads {
        validate_review_thread(thread)?;
    }

    Ok(())
}

/// Validates review thread.
fn validate_review_thread(thread: &ReviewThread) -> Result<(), ApiError> {
    validate_non_empty_review_field("threads[].id", &thread.id)?;
    validate_review_anchor(&thread.anchor)?;

    if thread.comments.is_empty() {
        return Err(ApiError::bad_request(
            "review threads must contain at least one comment",
        ));
    }

    for comment in &thread.comments {
        validate_non_empty_review_field("threads[].comments[].id", &comment.id)?;
        validate_non_empty_review_field("threads[].comments[].body", &comment.body)?;
        validate_non_empty_review_field("threads[].comments[].createdAt", &comment.created_at)?;
        validate_non_empty_review_field("threads[].comments[].updatedAt", &comment.updated_at)?;
    }

    Ok(())
}

/// Validates review anchor.
fn validate_review_anchor(anchor: &ReviewAnchor) -> Result<(), ApiError> {
    match anchor {
        ReviewAnchor::ChangeSet => Ok(()),
        ReviewAnchor::File { file_path } => {
            validate_non_empty_review_field("comments[].anchor.filePath", file_path)
        }
        ReviewAnchor::Hunk {
            file_path,
            hunk_header,
        } => {
            validate_non_empty_review_field("comments[].anchor.filePath", file_path)?;
            validate_non_empty_review_field("comments[].anchor.hunkHeader", hunk_header)
        }
        ReviewAnchor::Line {
            file_path,
            hunk_header,
            old_line,
            new_line,
        } => {
            validate_non_empty_review_field("comments[].anchor.filePath", file_path)?;
            validate_non_empty_review_field("comments[].anchor.hunkHeader", hunk_header)?;
            if old_line.is_none() && new_line.is_none() {
                return Err(ApiError::bad_request(
                    "line review anchors must include oldLine, newLine, or both",
                ));
            }
            Ok(())
        }
    }
}

/// Validates non empty review field.
fn validate_non_empty_review_field(label: &str, value: &str) -> Result<(), ApiError> {
    if value.trim().is_empty() {
        return Err(ApiError::bad_request(format!("{label} cannot be empty")));
    }

    Ok(())
}
