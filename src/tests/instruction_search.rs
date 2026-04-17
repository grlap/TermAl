// Instruction search traversal tests — phrase search returning all roots,
// directory discovery edge expansion, stopping at generic docs, transitive
// walks through instructionish docs, ignoring internal-only termal roots
// for Claude reviewer sessions, 404 for missing sessions, and the small
// `read_instruction_document` not-found check.
//
// Extracted from tests.rs — main cluster plus one scattered document
// helper test.

use super::*;

// Tests that instruction search returns all roots for a phrase.
#[test]
fn instruction_search_returns_all_roots_for_a_phrase() {
    let root = std::env::temp_dir().join(format!("termal-instruction-search-{}", Uuid::new_v4()));
    let docs_dir = root.join("docs");
    fs::create_dir_all(&docs_dir).unwrap();
    fs::write(
        root.join("AGENTS.md"),
        "See docs/backend.md for service rules.\n",
    )
    .unwrap();
    fs::write(
        root.join("CLAUDE.md"),
        "Use docs/backend.md for implementation guidance.\n",
    )
    .unwrap();
    fs::write(
        docs_dir.join("backend.md"),
        "# Backend\n\nPrefer dependency injection when module boundaries shift.\n",
    )
    .unwrap();

    let response = search_instruction_phrase(&root, "dependency injection").unwrap();

    assert_eq!(response.matches.len(), 1);
    let matched = &response.matches[0];
    assert!(
        matched.path.ends_with("docs\\backend.md") || matched.path.ends_with("docs/backend.md")
    );
    assert_eq!(
        matched.text,
        "Prefer dependency injection when module boundaries shift."
    );
    assert_eq!(matched.root_paths.len(), 2);
    assert_eq!(
        matched
            .root_paths
            .iter()
            .map(|root_path| root_path.root_path.clone())
            .collect::<Vec<_>>(),
        vec![
            normalize_path_best_effort(&root.join("AGENTS.md"))
                .to_string_lossy()
                .into_owned(),
            normalize_path_best_effort(&root.join("CLAUDE.md"))
                .to_string_lossy()
                .into_owned(),
        ]
    );
    assert!(
        matched
            .root_paths
            .iter()
            .all(|root_path| root_path.steps.len() == 1
                && root_path.steps[0].to_path == matched.path)
    );

    fs::remove_dir_all(root).unwrap();
}

// Tests that instruction search expands directory discovery edges.
#[test]
fn instruction_search_expands_directory_discovery_edges() {
    let root = std::env::temp_dir().join(format!(
        "termal-instruction-directory-search-{}",
        Uuid::new_v4()
    ));
    let reviewers_dir = root.join(".claude").join("reviewers");
    let commands_dir = root.join(".claude").join("commands");
    fs::create_dir_all(&reviewers_dir).unwrap();
    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("review-local.md"),
        "Discover reviewers in .claude/reviewers before running checks.\n",
    )
    .unwrap();
    fs::write(
        reviewers_dir.join("rust.md"),
        "Prefer dependency injection at unstable ownership boundaries.\n",
    )
    .unwrap();

    let response = search_instruction_phrase(&root, "dependency injection").unwrap();

    assert_eq!(response.matches.len(), 1);
    let matched = &response.matches[0];
    assert!(
        matched.path.ends_with(".claude\\reviewers\\rust.md")
            || matched.path.ends_with(".claude/reviewers/rust.md")
    );
    assert_eq!(matched.root_paths.len(), 1);
    let root_path = &matched.root_paths[0];
    assert_eq!(
        root_path.root_path,
        normalize_path_best_effort(&commands_dir.join("review-local.md"))
            .to_string_lossy()
            .into_owned()
    );
    assert_eq!(root_path.steps.len(), 1);
    assert_eq!(
        root_path.steps[0].relation,
        InstructionRelation::DirectoryDiscovery
    );
    assert_eq!(
        root_path.steps[0].to_path,
        normalize_path_best_effort(&reviewers_dir.join("rust.md"))
            .to_string_lossy()
            .into_owned()
    );

    fs::remove_dir_all(root).unwrap();
}

// Tests that instruction search stops at generic referenced docs.
#[test]
fn instruction_search_stops_at_generic_referenced_docs() {
    let root = std::env::temp_dir().join(format!(
        "termal-instruction-generic-docs-{}",
        Uuid::new_v4()
    ));
    let docs_dir = root.join("docs");
    let features_dir = docs_dir.join("features");
    fs::create_dir_all(&features_dir).unwrap();
    fs::write(
        root.join("AGENTS.md"),
        "See README.md for additional context.\n",
    )
    .unwrap();
    fs::write(
        root.join("README.md"),
        "- [docs/bugs.md](docs/bugs.md) - implementation backlog\n",
    )
    .unwrap();
    fs::write(
        docs_dir.join("bugs.md"),
        "- [Instruction Debugger](./features/instruction-debugger.md)\n",
    )
    .unwrap();
    fs::write(
        features_dir.join("instruction-debugger.md"),
        "Prefer dependency injection when debugging instruction graphs.\n",
    )
    .unwrap();

    let response = search_instruction_phrase(&root, "dependency injection").unwrap();

    assert!(response.matches.is_empty());

    fs::remove_dir_all(root).unwrap();
}

// Tests that instruction search walks instructionish docs transitively.
#[test]
fn instruction_search_walks_instructionish_docs_transitively() {
    let root =
        std::env::temp_dir().join(format!("termal-instruction-transitive-{}", Uuid::new_v4()));
    let instructions_dir = root.join("docs").join("instructions");
    fs::create_dir_all(&instructions_dir).unwrap();
    fs::write(
        root.join("AGENTS.md"),
        "Use docs/instructions/backend.md for service rules.\n",
    )
    .unwrap();
    fs::write(
        instructions_dir.join("backend.md"),
        "See shared.md for composition guidance.\n",
    )
    .unwrap();
    fs::write(
        instructions_dir.join("shared.md"),
        "Prefer dependency injection when module boundaries shift.\n",
    )
    .unwrap();

    let response = search_instruction_phrase(&root, "dependency injection").unwrap();

    assert_eq!(response.matches.len(), 1);
    let matched = &response.matches[0];
    assert!(
        matched.path.ends_with("docs\\instructions\\shared.md")
            || matched.path.ends_with("docs/instructions/shared.md")
    );
    assert_eq!(matched.root_paths.len(), 1);
    let root_path = &matched.root_paths[0];
    assert_eq!(
        root_path.root_path,
        normalize_path_best_effort(&root.join("AGENTS.md"))
            .to_string_lossy()
            .into_owned()
    );
    assert_eq!(root_path.steps.len(), 2);
    assert_eq!(
        root_path.steps[0].to_path,
        normalize_path_best_effort(&instructions_dir.join("backend.md"))
            .to_string_lossy()
            .into_owned()
    );
    assert_eq!(
        root_path.steps[1].to_path,
        normalize_path_best_effort(&instructions_dir.join("shared.md"))
            .to_string_lossy()
            .into_owned()
    );

    fs::remove_dir_all(root).unwrap();
}

// Tests that instruction search ignores internal TermAl roots for Claude reviewers.
#[test]
fn instruction_search_ignores_internal_termal_roots_for_claude_reviewers() {
    let root = std::env::temp_dir().join(format!(
        "termal-instruction-realtime-search-{}",
        Uuid::new_v4()
    ));
    let commands_dir = root.join(".claude").join("commands");
    let reviewers_dir = root.join(".claude").join("reviewers");
    let docs_features_dir = root.join("docs").join("features");
    let internal_skill_dir = root
        .join(".termal")
        .join("codex-home")
        .join("session-1")
        .join("skills")
        .join(".system")
        .join("skill-creator");
    fs::create_dir_all(&commands_dir).unwrap();
    fs::create_dir_all(&reviewers_dir).unwrap();
    fs::create_dir_all(&docs_features_dir).unwrap();
    fs::create_dir_all(&internal_skill_dir).unwrap();

    fs::write(
        commands_dir.join("review-local.md"),
        "Run `find .claude/reviewers -name \"*.md\" 2>/dev/null` via Bash to find all available reviewer lens files.\n",
    )
    .unwrap();
    fs::write(
        commands_dir.join("fix-bug.md"),
        "Read `docs/bugs.md` and find the matching bug entry.\n",
    )
    .unwrap();
    fs::write(
        reviewers_dir.join("react-typescript.md"),
        "5. **SSE / real-time handling**:\n",
    )
    .unwrap();
    fs::write(
        root.join("README.md"),
        "- [`docs/bugs.md`](docs/bugs.md) - implementation backlog\n",
    )
    .unwrap();
    fs::write(
        root.join("docs").join("bugs.md"),
        "- [Instruction Debugger](./features/instruction-debugger.md)\n",
    )
    .unwrap();
    fs::write(
        docs_features_dir.join("instruction-debugger.md"),
        "- a reviewer file was discovered from `.claude/reviewers/`\n",
    )
    .unwrap();
    fs::write(internal_skill_dir.join("SKILL.md"), "- README.md\n").unwrap();

    let response = search_instruction_phrase(&root, "real-time handling").unwrap();

    assert_eq!(response.matches.len(), 1);
    let matched = &response.matches[0];
    assert!(
        matched
            .path
            .ends_with(".claude\\reviewers\\react-typescript.md")
            || matched
                .path
                .ends_with(".claude/reviewers/react-typescript.md")
    );
    assert_eq!(matched.root_paths.len(), 1);
    let root_path = &matched.root_paths[0];
    assert_eq!(
        root_path.root_path,
        normalize_path_best_effort(&commands_dir.join("review-local.md"))
            .to_string_lossy()
            .into_owned()
    );
    assert_eq!(root_path.steps.len(), 1);
    assert_eq!(
        root_path.steps[0].relation,
        InstructionRelation::DirectoryDiscovery
    );
    assert_eq!(
        root_path.steps[0].to_path,
        normalize_path_best_effort(&reviewers_dir.join("react-typescript.md"))
            .to_string_lossy()
            .into_owned()
    );

    fs::remove_dir_all(root).unwrap();
}

// Tests that instruction search returns not found for missing session.
#[test]
fn instruction_search_returns_not_found_for_missing_session() {
    let state = test_app_state();
    let error = state
        .search_instructions("missing-session", "dependency injection")
        .unwrap_err();

    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert_eq!(error.message, "session not found");
}

// Tests that read instruction document returns not found for missing file.
#[test]
fn read_instruction_document_returns_not_found_for_missing_file() {
    let workdir =
        std::env::temp_dir().join(format!("termal-instruction-missing-{}", Uuid::new_v4()));
    let missing_file = workdir.join("AGENTS.md");

    fs::create_dir_all(&workdir).unwrap();

    let error = read_instruction_document(&missing_file, &workdir)
        .expect_err("missing instruction file should fail");

    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert!(error.message.contains("instruction file not found"));

    fs::remove_dir_all(workdir).unwrap();
}
