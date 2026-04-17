// Path resolution and normalization — session/project scoped lookups,
// canonicalization with creation-time parent checks, user-facing path
// prettification, project name defaulting, and related helpers used
// across the HTTP layer.
//
// Extracted from api.rs into its own `include!()` fragment so the FS-path
// plumbing is distinct from HTTP handler code.

/// Resolves requested path.
fn resolve_requested_path(path: &str) -> Result<PathBuf, ApiError> {
    let raw_path = FsPath::new(path);
    let resolved = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|err| ApiError::internal(format!("failed to resolve cwd: {err}")))?
            .join(raw_path)
    };

    Ok(resolved)
}

/// Resolves existing requested path.
fn resolve_existing_requested_path(path: &str, label: &str) -> Result<PathBuf, ApiError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request(format!("{label} cannot be empty")));
    }

    let requested_path = resolve_requested_path(trimmed)?;
    let resolved_path = canonicalize_existing_path(&requested_path, label)?;
    Ok(normalize_user_facing_path(&resolved_path))
}

/// Enumerates scoped path modes.
#[derive(Clone, Copy)]
enum ScopedPathMode {
    ExistingFile,
    ExistingPath,
    AllowMissingLeaf,
}

/// Normalizes optional identifier.
fn normalize_optional_identifier(value: Option<&str>) -> Option<&str> {
    value
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
}

/// Resolves session project root path.
fn resolve_session_project_root_path(
    state: &AppState,
    session_id: &str,
) -> Result<PathBuf, ApiError> {
    let root_path = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = &inner.sessions[index];
        if record.hidden {
            return Err(ApiError::not_found("session not found"));
        }
        if let Some(project) = record
            .session
            .project_id
            .as_deref()
            .and_then(|project_id| inner.find_project(project_id))
        {
            if project.remote_id != LOCAL_REMOTE_ID {
                return Err(ApiError::bad_request(format!(
                    "project `{}` is assigned to remote `{}`. Remote file access is not implemented yet.",
                    project.name, project.remote_id
                )));
            }
            project.root_path.clone()
        } else {
            inner
                .find_project_for_workdir(&record.session.workdir)
                .map(|project| project.root_path.clone())
                .unwrap_or_else(|| record.session.workdir.clone())
        }
    };

    fs::canonicalize(&root_path)
        .map(|path| normalize_user_facing_path(&path))
        .map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => {
                ApiError::bad_request(format!("project root not found: {root_path}"))
            }
            _ => ApiError::internal(format!(
                "failed to resolve project root {}: {err}",
                root_path
            )),
        })
}

/// Resolves project root path by ID.
fn resolve_project_root_path_by_id(
    state: &AppState,
    project_id: &str,
) -> Result<PathBuf, ApiError> {
    let root_path = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let project = inner
            .find_project(project_id)
            .ok_or_else(|| ApiError::not_found("project not found"))?;
        if project.remote_id != LOCAL_REMOTE_ID {
            return Err(ApiError::bad_request(format!(
                "project `{}` is assigned to remote `{}`. Remote file access is not implemented yet.",
                project.name, project.remote_id
            )));
        }
        project.root_path.clone()
    };

    fs::canonicalize(&root_path)
        .map(|path| normalize_user_facing_path(&path))
        .map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => {
                ApiError::bad_request(format!("project root not found: {root_path}"))
            }
            _ => ApiError::internal(format!(
                "failed to resolve project root {}: {err}",
                root_path
            )),
        })
}

/// Resolves request project root path.
fn resolve_request_project_root_path(
    state: &AppState,
    session_id: Option<&str>,
    project_id: Option<&str>,
) -> Result<PathBuf, ApiError> {
    if let Some(session_id) = normalize_optional_identifier(session_id) {
        return resolve_session_project_root_path(state, session_id);
    }

    if let Some(project_id) = normalize_optional_identifier(project_id) {
        return resolve_project_root_path_by_id(state, project_id);
    }

    Err(ApiError::bad_request("sessionId or projectId is required"))
}

/// Resolves project scoped requested path.
fn resolve_project_scoped_requested_path(
    state: &AppState,
    session_id: Option<&str>,
    project_id: Option<&str>,
    path: &str,
    mode: ScopedPathMode,
) -> Result<PathBuf, ApiError> {
    let project_root = resolve_request_project_root_path(state, session_id, project_id)?;
    let requested_path = resolve_requested_path(path)?;
    let resolved_path = match mode {
        ScopedPathMode::ExistingFile => canonicalize_existing_path(&requested_path, "file")?,
        ScopedPathMode::ExistingPath => canonicalize_existing_path(&requested_path, "path")?,
        ScopedPathMode::AllowMissingLeaf => {
            canonicalize_path_with_existing_ancestor(&requested_path)?
        }
    };

    if resolved_path != project_root && !resolved_path.starts_with(&project_root) {
        return Err(ApiError::bad_request(format!(
            "path `{}` must stay inside project `{}`",
            requested_path.display(),
            project_root.display()
        )));
    }

    Ok(normalize_user_facing_path(&resolved_path))
}

/// Resolves session scoped requested path.
#[cfg(test)]
fn resolve_session_scoped_requested_path(
    state: &AppState,
    session_id: &str,
    path: &str,
    mode: ScopedPathMode,
) -> Result<PathBuf, ApiError> {
    resolve_project_scoped_requested_path(state, Some(session_id), None, path, mode)
}

/// Canonicalizes existing path.
fn canonicalize_existing_path(path: &FsPath, label: &str) -> Result<PathBuf, ApiError> {
    fs::canonicalize(path)
        .map(|path| normalize_user_facing_path(&path))
        .map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => {
                ApiError::not_found(format!("{label} not found: {}", path.display()))
            }
            _ => ApiError::internal(format!(
                "failed to resolve {label} {}: {err}",
                path.display()
            )),
        })
}

/// Canonicalizes path with existing ancestor.
fn canonicalize_path_with_existing_ancestor(path: &FsPath) -> Result<PathBuf, ApiError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|err| ApiError::internal(format!("failed to resolve cwd: {err}")))?
            .join(path)
    };

    let mut suffix = Vec::<std::ffi::OsString>::new();
    let mut probe = absolute.as_path();

    loop {
        match fs::metadata(probe) {
            Ok(_) => {
                let mut canonical = fs::canonicalize(probe).map_err(|err| {
                    ApiError::internal(format!("failed to resolve path {}: {err}", probe.display()))
                })?;
                for component in suffix.iter().rev() {
                    validate_missing_path_component(component)?;
                    canonical.push(component);
                }
                return Ok(normalize_user_facing_path(&canonical));
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let Some(name) = probe.file_name().map(|value| value.to_os_string()) else {
                    return Err(ApiError::bad_request(format!(
                        "path not found: {}",
                        absolute.display()
                    )));
                };
                suffix.push(name);
                let Some(parent) = probe.parent() else {
                    return Err(ApiError::bad_request(format!(
                        "path not found: {}",
                        absolute.display()
                    )));
                };
                probe = parent;
            }
            Err(err) => {
                return Err(ApiError::internal(format!(
                    "failed to resolve path {}: {err}",
                    probe.display()
                )));
            }
        }
    }
}

/// Validates an unresolved path component before appending it to a canonical
/// ancestor. Missing suffixes must not contain traversal components because
/// containment checks run before the missing parent exists.
fn validate_missing_path_component(component: &std::ffi::OsStr) -> Result<(), ApiError> {
    if component.is_empty() || component == "." || component == ".." {
        return Err(ApiError::bad_request(
            "new file paths cannot contain unresolved `.` or `..` components",
        ));
    }

    Ok(())
}

/// Verifies the write parent after any missing directories are created.
fn verify_scoped_write_path_after_parent_creation(
    state: &AppState,
    session_id: Option<&str>,
    project_id: Option<&str>,
    resolved_path: &FsPath,
) -> Result<PathBuf, ApiError> {
    let project_root = resolve_request_project_root_path(state, session_id, project_id)?;
    let file_name = resolved_path.file_name().ok_or_else(|| {
        ApiError::bad_request(format!("file path is invalid: {}", resolved_path.display()))
    })?;
    validate_missing_path_component(file_name)?;
    let parent = resolved_path.parent().ok_or_else(|| {
        ApiError::bad_request(format!("file path is invalid: {}", resolved_path.display()))
    })?;
    let canonical_parent = fs::canonicalize(parent)
        .map(|path| normalize_user_facing_path(&path))
        .map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => {
                ApiError::not_found(format!("parent directory not found: {}", parent.display()))
            }
            _ => ApiError::internal(format!(
                "failed to resolve parent directory {}: {err}",
                parent.display()
            )),
        })?;

    if canonical_parent != project_root && !canonical_parent.starts_with(&project_root) {
        return Err(ApiError::bad_request(format!(
            "path `{}` must stay inside project `{}`",
            resolved_path.display(),
            project_root.display()
        )));
    }

    Ok(normalize_user_facing_path(&canonical_parent.join(file_name)))
}

/// Normalizes user facing path.
fn normalize_user_facing_path(path: &FsPath) -> PathBuf {
    #[cfg(windows)]
    {
        let raw = path.to_string_lossy();
        if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = raw.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }

    path.to_path_buf()
}

/// Resolves directory path.
fn resolve_directory_path(path: &str, label: &str) -> Result<String, ApiError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request(format!("{label} cannot be empty")));
    }

    let resolved = resolve_requested_path(trimmed)?;
    let directory = if resolved.is_dir() {
        resolved
    } else {
        return Err(ApiError::bad_request(format!(
            "`{}` is not a directory",
            trimmed
        )));
    };
    let canonical = fs::canonicalize(&directory).unwrap_or(directory);
    Ok(normalize_user_facing_path(&canonical)
        .to_string_lossy()
        .into_owned())
}

/// Resolves project root path.
fn resolve_project_root_path(path: &str) -> Result<String, ApiError> {
    resolve_directory_path(path, "project root path")
}

/// Resolves session working directory.
fn resolve_session_workdir(path: &str) -> Result<String, ApiError> {
    resolve_directory_path(path, "session workdir")
}

/// Picks project root path.
fn pick_project_root_path(default_workdir: &str) -> Result<Option<String>, ApiError> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("osascript")
            .arg("-e")
            .arg("on run argv")
            .arg("-e")
            .arg("set defaultLocation to POSIX file (item 1 of argv)")
            .arg("-e")
            .arg("try")
            .arg("-e")
            .arg(
                "set chosenFolder to choose folder with prompt \"Choose a folder for this project\" default location defaultLocation",
            )
            .arg("-e")
            .arg("return POSIX path of chosenFolder")
            .arg("-e")
            .arg("on error number -128")
            .arg("-e")
            .arg("return \"\"")
            .arg("-e")
            .arg("end try")
            .arg("-e")
            .arg("end run")
            .arg(default_workdir)
            .output()
            .map_err(|err| ApiError::internal(format!("failed to open folder picker: {err}")))?;

        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            let message = if detail.is_empty() {
                "folder picker failed".to_owned()
            } else {
                format!("folder picker failed: {detail}")
            };
            return Err(ApiError::internal(message));
        }

        let selected = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if selected.is_empty() {
            return Ok(None);
        }

        return resolve_project_root_path(&selected).map(Some);
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = default_workdir;
        Err(ApiError::bad_request(
            "Folder picker is unavailable on this platform. Enter the path manually.",
        ))
    }
}

/// Returns the default project name.
fn default_project_name(root_path: &str) -> String {
    let path = FsPath::new(root_path);
    path.file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| root_path.to_owned())
}

/// Deduplicates project name.
fn dedupe_project_name(existing: &[Project], base_name: &str) -> String {
    let existing_names = existing
        .iter()
        .map(|project| project.name.as_str())
        .collect::<HashSet<_>>();
    if !existing_names.contains(base_name) {
        return base_name.to_owned();
    }

    let mut suffix = 2usize;
    loop {
        let candidate = format!("{base_name} {suffix}");
        if !existing_names.contains(candidate.as_str()) {
            return candidate;
        }
        suffix += 1;
    }
}

/// Handles path contains.
fn path_contains(root_path: &str, candidate_path: &FsPath) -> bool {
    let root = normalize_path_best_effort(FsPath::new(root_path));
    let candidate = normalize_path_best_effort(candidate_path);
    candidate == root || candidate.starts_with(root)
}

/// Normalizes path best effort.
fn normalize_path_best_effort(path: &FsPath) -> PathBuf {
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let canonical = fs::canonicalize(&resolved).unwrap_or(resolved);
    normalize_user_facing_path(&canonical)
}
