/*
Instruction search graph traversal.

Pure helpers used by the `/api/instructions` endpoint: phrase search,
seed discovery, reference sanitization, transitive-edge tracing, document
classification, and path normalization for Claude / Codex / Gemini
instruction files.

The HTTP handler (`search_instructions`) and the request/response structs
(`InstructionSearchQuery`, `InstructionSearchResponse`, ...) stay in
`api.rs`. This file owns only the traversal logic; everything is visible
across the flat `include!()`-assembled module.
*/

/// Searches instruction phrase.
fn search_instruction_phrase(
    workdir: &FsPath,
    query: &str,
) -> Result<InstructionSearchResponse, ApiError> {
    let trimmed_query = query.trim();
    if trimmed_query.is_empty() {
        return Err(ApiError::bad_request(
            "instruction search query cannot be empty",
        ));
    }

    let graph = build_instruction_search_graph(workdir)?;
    let normalized_query = trimmed_query.to_ascii_lowercase();
    let mut matches = Vec::new();
    let mut documents = graph.documents.values().collect::<Vec<_>>();
    documents.sort_by(|left, right| {
        left.path
            .to_string_lossy()
            .to_ascii_lowercase()
            .cmp(&right.path.to_string_lossy().to_ascii_lowercase())
    });

    for document in documents {
        for (line_index, line) in document.lines.iter().enumerate() {
            if !line.to_ascii_lowercase().contains(&normalized_query) {
                continue;
            }

            matches.push(InstructionSearchMatch {
                line: line_index + 1,
                path: document.path.to_string_lossy().into_owned(),
                root_paths: trace_instruction_roots(&graph, &document.path),
                text: line.trim().to_owned(),
            });
        }
    }

    matches.sort_by(|left, right| {
        left.path
            .to_ascii_lowercase()
            .cmp(&right.path.to_ascii_lowercase())
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| {
                left.text
                    .to_ascii_lowercase()
                    .cmp(&right.text.to_ascii_lowercase())
            })
    });

    Ok(InstructionSearchResponse {
        matches,
        query: trimmed_query.to_owned(),
        workdir: workdir.to_string_lossy().into_owned(),
    })
}

/// Builds instruction search graph.
fn build_instruction_search_graph(workdir: &FsPath) -> Result<InstructionSearchGraph, ApiError> {
    let normalized_workdir = normalize_path_best_effort(workdir);
    let seed_paths = discover_instruction_seed_paths(&normalized_workdir)?;
    let mut documents = HashMap::new();
    let mut outgoing = HashMap::<String, Vec<InstructionPathStep>>::new();
    let mut queued = HashSet::new();
    let mut pending = VecDeque::new();

    for path in seed_paths {
        let normalized = normalize_path_best_effort(&path);
        let normalized_string = normalized.to_string_lossy().into_owned();
        if queued.insert(normalized_string.clone()) {
            pending.push_back(normalized);
        }
    }

    while let Some(path) = pending.pop_front() {
        let normalized_path = normalize_path_best_effort(&path);
        let normalized_path_string = normalized_path.to_string_lossy().into_owned();
        if documents.contains_key(&normalized_path_string) {
            continue;
        }

        let document = read_instruction_document(&normalized_path, &normalized_workdir)?;
        let edges = extract_instruction_edges(&document, &normalized_workdir)?;
        for edge in &edges {
            if queued.insert(edge.to_path.clone()) {
                pending.push_back(PathBuf::from(&edge.to_path));
            }
        }

        outgoing.insert(normalized_path_string.clone(), edges);
        documents.insert(normalized_path_string, document);
    }

    let mut incoming = HashMap::<String, Vec<InstructionPathStep>>::new();
    for steps in outgoing.values() {
        for step in steps {
            incoming
                .entry(step.to_path.clone())
                .or_default()
                .push(step.clone());
        }
    }

    for steps in outgoing.values_mut() {
        steps.sort_by(|left, right| {
            left.from_path
                .to_ascii_lowercase()
                .cmp(&right.from_path.to_ascii_lowercase())
                .then_with(|| left.line.cmp(&right.line))
                .then_with(|| {
                    left.to_path
                        .to_ascii_lowercase()
                        .cmp(&right.to_path.to_ascii_lowercase())
                })
        });
    }

    for steps in incoming.values_mut() {
        steps.sort_by(|left, right| {
            left.from_path
                .to_ascii_lowercase()
                .cmp(&right.from_path.to_ascii_lowercase())
                .then_with(|| left.line.cmp(&right.line))
                .then_with(|| {
                    left.to_path
                        .to_ascii_lowercase()
                        .cmp(&right.to_path.to_ascii_lowercase())
                })
        });
    }

    Ok(InstructionSearchGraph {
        documents,
        incoming,
    })
}

/// Handles discover instruction seed paths.
fn discover_instruction_seed_paths(workdir: &FsPath) -> Result<Vec<PathBuf>, ApiError> {
    let mut pending_directories = vec![workdir.to_path_buf()];
    let mut paths = Vec::new();

    while let Some(directory) = pending_directories.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(ApiError::internal(format!(
                    "failed to read instruction directory {}: {err}",
                    directory.display()
                )));
            }
        };

        for entry in entries {
            let entry = entry.map_err(|err| {
                ApiError::internal(format!(
                    "failed to read instruction directory entry in {}: {err}",
                    directory.display()
                ))
            })?;
            let path = entry.path();
            let metadata = entry.metadata().map_err(|err| {
                ApiError::internal(format!(
                    "failed to stat instruction path {}: {err}",
                    path.display()
                ))
            })?;

            if metadata.is_dir() {
                if should_skip_instruction_directory(&path) {
                    continue;
                }
                pending_directories.push(path);
                continue;
            }

            if is_instruction_seed_path(&path, workdir) {
                paths.push(path);
            }
        }
    }

    paths.sort_by(|left, right| {
        left.to_string_lossy()
            .to_ascii_lowercase()
            .cmp(&right.to_string_lossy().to_ascii_lowercase())
    });
    paths.dedup_by(|left, right| {
        normalize_path_best_effort(left) == normalize_path_best_effort(right)
    });
    Ok(paths)
}

/// Returns whether skip instruction directory.
fn should_skip_instruction_directory(path: &FsPath) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    should_skip_instruction_directory_name(name)
}

/// Returns whether skip instruction directory name.
fn should_skip_instruction_directory_name(name: &str) -> bool {
    matches!(
        name,
        ".git" | ".idea" | ".termal" | "node_modules" | "target" | "dist" | "build" | ".next"
    )
}

/// Returns whether instruction seed path.
fn is_instruction_seed_path(path: &FsPath, workdir: &FsPath) -> bool {
    if !path.is_file() {
        return false;
    }

    let normalized = normalize_path_best_effort(path);
    if !path_contains(&workdir.to_string_lossy(), &normalized)
        || path_is_in_skipped_instruction_directory(&normalized, workdir)
    {
        return false;
    }

    let lower_file_name = normalized
        .file_name()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());
    let lower_relative = normalized
        .strip_prefix(workdir)
        .ok()
        .map(|value| {
            value
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
        })
        .unwrap_or_else(|| {
            normalized
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
        });

    if matches!(
        lower_file_name.as_deref(),
        Some("agents.md")
            | Some("claude.md")
            | Some("gemini.md")
            | Some("skills.md")
            | Some("skill.md")
            | Some("rules.md")
            | Some("agent.md")
            | Some(".claude.md")
    ) {
        return true;
    }

    if lower_relative.starts_with(".claude/commands/") && lower_relative.ends_with(".md") {
        return true;
    }

    if lower_relative.starts_with(".claude/reviewers/") && lower_relative.ends_with(".md") {
        return true;
    }

    if lower_relative == ".cursor/rules" {
        return true;
    }

    if lower_relative.starts_with(".cursor/rules/")
        && (lower_relative.ends_with(".md") || lower_relative.ends_with(".mdc"))
    {
        return true;
    }

    normalized
        .ancestors()
        .skip(1)
        .filter_map(|ancestor| ancestor.file_name().and_then(|value| value.to_str()))
        .map(|value| value.to_ascii_lowercase())
        .any(|value| value == "skills" || value == "rules")
        && lower_relative.ends_with(".md")
}

/// Reads instruction document.
fn read_instruction_document(
    path: &FsPath,
    workdir: &FsPath,
) -> Result<InstructionDocumentInternal, ApiError> {
    let content = fs::read_to_string(path).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => {
            ApiError::not_found(format!("instruction file not found: {}", path.display()))
        }
        io::ErrorKind::InvalidData => ApiError::bad_request(format!(
            "instruction file is not valid UTF-8: {}",
            path.display()
        )),
        _ => ApiError::internal(format!(
            "failed to read instruction file {}: {err}",
            path.display()
        )),
    })?;

    Ok(InstructionDocumentInternal {
        kind: classify_instruction_document_kind(path, workdir),
        lines: content.lines().map(str::to_owned).collect(),
        path: path.to_path_buf(),
    })
}

/// Classifies instruction document kind.
fn classify_instruction_document_kind(path: &FsPath, workdir: &FsPath) -> InstructionDocumentKind {
    let lower_file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());
    let lower_relative = path
        .strip_prefix(workdir)
        .ok()
        .map(|value| {
            value
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
        })
        .unwrap_or_else(|| {
            path.to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
        });

    if matches!(lower_file_name.as_deref(), Some("skill.md")) {
        return InstructionDocumentKind::SkillInstruction;
    }

    if matches!(
        lower_file_name.as_deref(),
        Some("agents.md")
            | Some("claude.md")
            | Some("gemini.md")
            | Some("rules.md")
            | Some("skills.md")
            | Some("agent.md")
            | Some(".claude.md")
    ) {
        return InstructionDocumentKind::RootInstruction;
    }

    if lower_relative.starts_with(".claude/commands/") {
        return InstructionDocumentKind::CommandInstruction;
    }

    if lower_relative.starts_with(".claude/reviewers/") {
        return InstructionDocumentKind::ReviewerInstruction;
    }

    if lower_relative.starts_with(".cursor/rules/")
        || path
            .ancestors()
            .skip(1)
            .filter_map(|ancestor| ancestor.file_name().and_then(|value| value.to_str()))
            .any(|value| {
                value.eq_ignore_ascii_case("rules")
                    || value.eq_ignore_ascii_case("instructions")
                    || value.eq_ignore_ascii_case("agents")
            })
    {
        return InstructionDocumentKind::RulesInstruction;
    }

    if path
        .ancestors()
        .skip(1)
        .filter_map(|ancestor| ancestor.file_name().and_then(|value| value.to_str()))
        .any(|value| value.eq_ignore_ascii_case("skills"))
    {
        return InstructionDocumentKind::SkillInstruction;
    }

    InstructionDocumentKind::ReferencedInstruction
}

/// Extracts instruction edges.
fn extract_instruction_edges(
    document: &InstructionDocumentInternal,
    workdir: &FsPath,
) -> Result<Vec<InstructionPathStep>, ApiError> {
    if !supports_transitive_instruction_edges(document.kind) {
        return Ok(Vec::new());
    }

    let mut edges = Vec::new();
    let mut seen = HashSet::new();

    for (line_index, line) in document.lines.iter().enumerate() {
        let line_number = line_index + 1;

        for raw_target in extract_markdown_link_targets(line) {
            maybe_push_instruction_file_edge(
                &mut edges,
                &mut seen,
                document,
                line_number,
                line,
                workdir,
                &raw_target,
                InstructionRelation::MarkdownLink,
            );
        }

        for raw_target in extract_instruction_path_tokens(line) {
            maybe_push_instruction_file_edge(
                &mut edges,
                &mut seen,
                document,
                line_number,
                line,
                workdir,
                &raw_target,
                InstructionRelation::FileReference,
            );
        }

        for raw_directory in [".claude/reviewers", ".claude/commands", ".cursor/rules"] {
            if !line.contains(raw_directory) {
                continue;
            }

            if let Some(directory) =
                resolve_instruction_reference_directory(&document.path, workdir, raw_directory)
            {
                let markdown_files = collect_markdown_files_in_directory(&directory)?;
                for markdown_file in markdown_files {
                    maybe_push_instruction_edge(
                        &mut edges,
                        &mut seen,
                        line_number,
                        line,
                        &document.path,
                        &markdown_file,
                        InstructionRelation::DirectoryDiscovery,
                    );
                }
            }
        }
    }

    Ok(edges)
}

/// Handles maybe push instruction file edge.
fn maybe_push_instruction_file_edge(
    edges: &mut Vec<InstructionPathStep>,
    seen: &mut HashSet<(String, usize, InstructionRelation)>,
    document: &InstructionDocumentInternal,
    line_number: usize,
    line: &str,
    workdir: &FsPath,
    raw_target: &str,
    relation: InstructionRelation,
) {
    let Some(target_path) = resolve_instruction_reference_file(&document.path, workdir, raw_target)
    else {
        return;
    };

    maybe_push_instruction_edge(
        edges,
        seen,
        line_number,
        line,
        &document.path,
        &target_path,
        relation,
    );
}

/// Handles maybe push instruction edge.
fn maybe_push_instruction_edge(
    edges: &mut Vec<InstructionPathStep>,
    seen: &mut HashSet<(String, usize, InstructionRelation)>,
    line_number: usize,
    line: &str,
    from_path: &FsPath,
    to_path: &FsPath,
    relation: InstructionRelation,
) {
    let normalized_from = normalize_path_best_effort(from_path)
        .to_string_lossy()
        .into_owned();
    let normalized_to = normalize_path_best_effort(to_path)
        .to_string_lossy()
        .into_owned();
    if normalized_from == normalized_to {
        return;
    }

    let dedupe_key = (normalized_to.clone(), line_number, relation);
    if !seen.insert(dedupe_key) {
        return;
    }

    edges.push(InstructionPathStep {
        excerpt: line.trim().to_owned(),
        from_path: normalized_from,
        line: line_number,
        relation,
        to_path: normalized_to,
    });
}

/// Resolves instruction reference file.
fn resolve_instruction_reference_file(
    source_path: &FsPath,
    workdir: &FsPath,
    raw_target: &str,
) -> Option<PathBuf> {
    let sanitized_target = sanitize_instruction_reference(raw_target);
    if sanitized_target.is_empty() {
        return None;
    }

    let path_only = sanitized_target
        .split_once('#')
        .map(|(prefix, _)| prefix)
        .unwrap_or(sanitized_target.as_str())
        .split_once('?')
        .map(|(prefix, _)| prefix)
        .unwrap_or(sanitized_target.as_str())
        .trim();
    if path_only.is_empty() {
        return None;
    }

    for candidate in instruction_reference_candidates(source_path, workdir, path_only) {
        if !candidate.is_file() {
            continue;
        }

        let lower_name = candidate
            .file_name()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase());
        let extension = candidate.extension().and_then(|value| value.to_str());
        if matches!(extension, Some("md") | Some("mdc"))
            || matches!(lower_name.as_deref(), Some("rules") | Some(".claude"))
        {
            return Some(candidate);
        }
    }

    None
}

/// Resolves instruction reference directory.
fn resolve_instruction_reference_directory(
    source_path: &FsPath,
    workdir: &FsPath,
    raw_target: &str,
) -> Option<PathBuf> {
    let sanitized_target = sanitize_instruction_reference(raw_target);
    if sanitized_target.is_empty() {
        return None;
    }

    instruction_reference_candidates(source_path, workdir, &sanitized_target)
        .into_iter()
        .find(|candidate| candidate.is_dir())
}

/// Handles instruction reference candidates.
fn instruction_reference_candidates(
    source_path: &FsPath,
    workdir: &FsPath,
    raw_target: &str,
) -> Vec<PathBuf> {
    let target_path = FsPath::new(raw_target);
    let mut candidates = Vec::new();

    if target_path.is_absolute() {
        let normalized = normalize_path_best_effort(target_path);
        if path_contains(&workdir.to_string_lossy(), &normalized)
            && !path_is_in_skipped_instruction_directory(&normalized, workdir)
        {
            candidates.push(normalized);
        }
        return candidates;
    }

    if let Some(parent) = source_path.parent() {
        let normalized = normalize_path_best_effort(&parent.join(target_path));
        if path_contains(&workdir.to_string_lossy(), &normalized)
            && !path_is_in_skipped_instruction_directory(&normalized, workdir)
        {
            candidates.push(normalized);
        }
    }

    let workdir_relative = normalize_path_best_effort(&workdir.join(target_path));
    if path_contains(&workdir.to_string_lossy(), &workdir_relative)
        && !path_is_in_skipped_instruction_directory(&workdir_relative, workdir)
        && !candidates
            .iter()
            .any(|candidate| candidate == &workdir_relative)
    {
        candidates.push(workdir_relative);
    }

    candidates
}

/// Extracts markdown link targets.
fn extract_markdown_link_targets(line: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut start = 0usize;

    while let Some(relative_open) = line[start..].find("](") {
        let target_start = start + relative_open + 2;
        let Some(relative_close) = line[target_start..].find(')') else {
            break;
        };
        let target_end = target_start + relative_close;
        targets.push(line[target_start..target_end].to_owned());
        start = target_end + 1;
    }

    targets
}

/// Extracts instruction path tokens.
fn extract_instruction_path_tokens(line: &str) -> Vec<String> {
    line.split_whitespace()
        .map(sanitize_instruction_reference)
        .filter(|token| {
            if token.is_empty() {
                return false;
            }

            let lower = token.to_ascii_lowercase();
            lower.contains(".md")
                || lower.contains(".mdc")
                || matches!(
                    lower.as_str(),
                    "agents.md"
                        | "claude.md"
                        | "gemini.md"
                        | "skills.md"
                        | "skill.md"
                        | "rules.md"
                        | "agent.md"
                        | ".claude.md"
                        | ".cursor/rules"
                )
        })
        .collect()
}

/// Sanitizes instruction reference.
fn sanitize_instruction_reference(raw: &str) -> String {
    raw.trim()
        .trim_matches(|character: char| {
            matches!(
                character,
                '`' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';' | ':'
            )
        })
        .trim_end_matches('.')
        .trim()
        .to_owned()
}

/// Collects markdown files in directory.
fn collect_markdown_files_in_directory(directory: &FsPath) -> Result<Vec<PathBuf>, ApiError> {
    let mut pending_directories = vec![directory.to_path_buf()];
    let mut paths = Vec::new();

    while let Some(current) = pending_directories.pop() {
        let entries = match fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(ApiError::internal(format!(
                    "failed to read instruction directory {}: {err}",
                    current.display()
                )));
            }
        };

        for entry in entries {
            let entry = entry.map_err(|err| {
                ApiError::internal(format!(
                    "failed to read instruction directory entry in {}: {err}",
                    current.display()
                ))
            })?;
            let path = entry.path();
            let metadata = entry.metadata().map_err(|err| {
                ApiError::internal(format!(
                    "failed to stat instruction directory path {}: {err}",
                    path.display()
                ))
            })?;

            if metadata.is_dir() {
                if should_skip_instruction_directory(&path) {
                    continue;
                }
                pending_directories.push(path);
                continue;
            }

            if matches!(
                path.extension().and_then(|value| value.to_str()),
                Some("md") | Some("mdc")
            ) {
                paths.push(normalize_path_best_effort(&path));
            }
        }
    }

    paths.sort_by(|left, right| {
        left.to_string_lossy()
            .to_ascii_lowercase()
            .cmp(&right.to_string_lossy().to_ascii_lowercase())
    });
    paths.dedup();
    Ok(paths)
}

/// Returns whether transitive instruction edges.
fn supports_transitive_instruction_edges(kind: InstructionDocumentKind) -> bool {
    !matches!(kind, InstructionDocumentKind::ReferencedInstruction)
}

/// Handles path is in skipped instruction directory.
fn path_is_in_skipped_instruction_directory(path: &FsPath, workdir: &FsPath) -> bool {
    let normalized = normalize_path_best_effort(path);
    let Ok(relative) = normalized.strip_prefix(workdir) else {
        return false;
    };

    relative.components().any(|component| match component {
        std::path::Component::Normal(value) => value
            .to_str()
            .map(should_skip_instruction_directory_name)
            .unwrap_or(false),
        _ => false,
    })
}

/// Traces instruction roots.
fn trace_instruction_roots(
    graph: &InstructionSearchGraph,
    target_path: &FsPath,
) -> Vec<InstructionRootPath> {
    let normalized_target = normalize_path_best_effort(target_path);
    let normalized_target_string = normalized_target.to_string_lossy().into_owned();
    let mut results = Vec::new();
    let mut current_steps = Vec::new();
    let mut visited = HashSet::new();

    trace_instruction_roots_recursive(
        graph,
        &normalized_target_string,
        &mut current_steps,
        &mut visited,
        &mut results,
    );

    results.sort_by(|left, right| {
        left.root_path
            .to_ascii_lowercase()
            .cmp(&right.root_path.to_ascii_lowercase())
            .then_with(|| left.steps.len().cmp(&right.steps.len()))
    });
    results.dedup_by(|left, right| {
        left.root_path == right.root_path
            && left.root_kind == right.root_kind
            && left.steps == right.steps
    });
    results
}

/// Traces instruction roots recursive.
fn trace_instruction_roots_recursive(
    graph: &InstructionSearchGraph,
    current_path: &str,
    current_steps: &mut Vec<InstructionPathStep>,
    visited: &mut HashSet<String>,
    results: &mut Vec<InstructionRootPath>,
) {
    if !visited.insert(current_path.to_owned()) {
        return;
    }

    let incoming = graph
        .incoming
        .get(current_path)
        .cloned()
        .unwrap_or_default();
    if incoming.is_empty() {
        if let Some(document) = graph.documents.get(current_path) {
            let mut steps = current_steps.clone();
            steps.reverse();
            results.push(InstructionRootPath {
                root_kind: document.kind,
                root_path: document.path.to_string_lossy().into_owned(),
                steps,
            });
        }
        visited.remove(current_path);
        return;
    }

    for edge in incoming {
        current_steps.push(edge.clone());
        trace_instruction_roots_recursive(graph, &edge.from_path, current_steps, visited, results);
        current_steps.pop();
    }

    visited.remove(current_path);
}
