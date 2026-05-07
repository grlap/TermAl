//! Tests for TermAl's git diff feature, which powers `/api/git/diff`: a
//! request names a file under a session's workdir and gets back a
//! structured response with the raw patch text (`diff`), the change
//! classification, and — for Markdown files — optional
//! `document_content` holding the before/after sides as full UTF-8
//! strings so the UI can render a rendered-Markdown diff view instead
//! of a raw unified patch.
//!
//! Document enrichment is best-effort: when a side cannot be loaded
//! (oversized worktree file, committed object above the read ceiling,
//! non-UTF-8 bytes, directory or missing entry, symlink pointing
//! outside the repo), the response degrades to the raw patch plus a
//! user-facing `document_enrichment_note` explaining why the rendered
//! view is unavailable. Notes are keyed off structured `ApiErrorKind`
//! tags with a small whitelist of legacy status-only fallbacks.
//!
//! Security invariants pinned here: `read_git_worktree_text` must
//! canonicalize through symlinks without letting either a symlinked
//! leaf or a symlinked parent escape the repo root (five unix-only
//! tests), and every document reader caps at `MAX_FILE_CONTENT_BYTES`
//! so a large committed blob or worktree file cannot blow up the
//! process. `parse_git_status_paths` decodes git's C-escaped quoted
//! paths (`"folder/file with spaces.txt"`, `"caf\303\251.txt"`, and
//! `"old" -> "new"` rename arrows) so non-ASCII and space-containing
//! paths survive the status pipeline.
//!
//! All production surfaces live in `src/git.rs` (extracted earlier
//! this session): `load_git_diff_for_request`,
//! `load_git_diff_document_content`, `read_git_diff_document_side`,
//! `read_git_worktree_text`, `git_diff_document_enrichment_note`,
//! `should_degrade_git_diff_document_enrichment_error`,
//! `parse_git_status_paths`, `push_git_repo`, and `sync_git_repo`.

use super::*;

// Pins git's C-escaped quoted status paths to decoded owned strings.
// Guards against losing spaces, non-ASCII octals (`\303\251` → é), or
// rename arrows when the status pipeline hands paths to pathspec and
// diff callers.
#[test]
fn parses_quoted_git_status_paths() {
    assert_eq!(
        parse_git_status_paths(r#""folder/file with spaces.txt""#),
        (None, "folder/file with spaces.txt".to_owned())
    );
    assert_eq!(
        parse_git_status_paths(r#""caf\303\251.txt""#),
        (None, "caf\u{00e9}.txt".to_owned())
    );
    assert_eq!(
        parse_git_status_paths(r#""old name.txt" -> "new name.txt""#),
        (Some("old name.txt".to_owned()), "new name.txt".to_owned(),)
    );
}

// Pins the end-to-end status → add → restore flow for paths containing
// spaces. Guards against regressions where a space-containing path
// round-trips through git status but fails to stage/unstage because
// pathspec quoting was dropped on the way back out.
#[test]
fn git_status_file_actions_support_paths_with_spaces() {
    let repo_root = std::env::temp_dir().join(format!("termal-git-status-{}", Uuid::new_v4()));
    let nested_dir = repo_root.join("folder");
    let tracked_file = repo_root.join("README.md");
    let spaced_file = nested_dir.join("file with spaces.txt");

    fs::create_dir_all(&nested_dir).unwrap();
    fs::write(&tracked_file, "# Test\n").unwrap();

    run_git_test_command(&repo_root, &["init"]);
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);
    run_git_test_command(&repo_root, &["add", "README.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);

    fs::write(&spaced_file, "hello\n").unwrap();

    let status = load_git_status_for_path(&repo_root).unwrap();
    let file = status
        .files
        .iter()
        .find(|entry| entry.path == "folder/file with spaces.txt")
        .expect("status should include the untracked file");

    assert_eq!(file.index_status.as_deref(), Some("?"));
    assert_eq!(file.worktree_status.as_deref(), Some("?"));

    let pathspecs = collect_git_pathspecs(&file.path, None);
    run_git_pathspec_command(
        &repo_root,
        &["add", "-A"],
        &pathspecs,
        "failed to stage git changes",
    )
    .unwrap();

    let staged_status = load_git_status_for_path(&repo_root).unwrap();
    let staged_file = staged_status
        .files
        .iter()
        .find(|entry| entry.path == "folder/file with spaces.txt")
        .expect("status should include the staged file");

    assert_eq!(staged_file.index_status.as_deref(), Some("A"));
    assert_eq!(staged_file.worktree_status, None);

    run_git_pathspec_command(
        &repo_root,
        &["restore", "--staged"],
        &pathspecs,
        "failed to unstage git changes",
    )
    .unwrap();

    let unstaged_status = load_git_status_for_path(&repo_root).unwrap();
    let unstaged_file = unstaged_status
        .files
        .iter()
        .find(|entry| entry.path == "folder/file with spaces.txt")
        .expect("status should include the unstaged file");

    assert_eq!(unstaged_file.index_status.as_deref(), Some("?"));
    assert_eq!(unstaged_file.worktree_status.as_deref(), Some("?"));

    fs::remove_dir_all(repo_root).unwrap();
}

// Pins staging a worktree edit made after a staged rename. Git reports
// this as `RM old -> new`, but `git add` must receive only the current
// path for the unstaged modification; the old path no longer matches.
#[test]
fn git_stage_action_supports_unstaged_edit_on_staged_rename() {
    let repo_root =
        std::env::temp_dir().join(format!("termal-git-stage-rename-{}", Uuid::new_v4()));
    let status_dir = repo_root.join("docs").join("status");
    let old_file = status_dir.join("gdpr.md");
    let new_file = status_dir.join("legal.md");

    fs::create_dir_all(&status_dir).unwrap();
    fs::write(&old_file, "# GDPR\n\nBase text.\n").unwrap();
    init_git_document_test_repo(&repo_root);
    run_git_test_command(&repo_root, &["add", "docs/status/gdpr.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);

    run_git_test_command(
        &repo_root,
        &["mv", "docs/status/gdpr.md", "docs/status/legal.md"],
    );
    fs::write(&new_file, "# GDPR\n\nBase text.\n\nWorktree edit.\n").unwrap();

    let status = load_git_status_for_path(&repo_root).unwrap();
    let file = status
        .files
        .iter()
        .find(|entry| entry.path == "docs/status/legal.md")
        .expect("status should include the renamed file");

    assert_eq!(file.original_path.as_deref(), Some("docs/status/gdpr.md"));
    assert_eq!(file.index_status.as_deref(), Some("R"));
    assert_eq!(file.worktree_status.as_deref(), Some("M"));

    let pathspecs = collect_git_stage_pathspecs(
        &file.path,
        file.original_path.as_deref(),
        file.worktree_status.as_deref(),
    );
    assert_eq!(pathspecs, vec!["docs/status/legal.md".to_owned()]);

    run_git_pathspec_command(
        &repo_root,
        &["add", "-A"],
        &pathspecs,
        "failed to stage git changes",
    )
    .unwrap();

    let staged_status = load_git_status_for_path(&repo_root).unwrap();
    assert!(
        staged_status
            .files
            .iter()
            .all(|entry| entry.worktree_status.is_none())
    );

    fs::remove_dir_all(repo_root).unwrap();
}

// Pins the staged/unstaged side mapping for Markdown enrichment:
// staged reads HEAD → index, unstaged reads index → worktree, and a
// staged view is marked read-only when the worktree has unstaged
// changes. Guards against the UI editing stale staged content or
// conflating the two sides.
#[test]
fn git_diff_document_content_uses_selected_git_side_for_markdown() {
    let repo_root = std::env::temp_dir().join(format!("termal-git-diff-doc-{}", Uuid::new_v4()));
    let markdown_file = repo_root.join("README.md");

    fs::create_dir_all(&repo_root).unwrap();
    fs::write(&markdown_file, "# Base\n\nInitial text.\n").unwrap();

    run_git_test_command(&repo_root, &["init"]);
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);
    run_git_test_command(&repo_root, &["add", "README.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);

    fs::write(&markdown_file, "# Staged\n\nReady to commit.\n").unwrap();
    run_git_test_command(&repo_root, &["add", "README.md"]);
    let clean_staged = load_git_diff_for_request(
        &repo_root,
        &GitDiffRequest {
            original_path: None,
            path: "README.md".to_owned(),
            section_id: GitDiffSection::Staged,
            status_code: Some("M".to_owned()),
            workdir: repo_root.to_string_lossy().into_owned(),
            project_id: None,
            session_id: None,
        },
    )
    .unwrap();
    let clean_staged_document = clean_staged
        .document_content
        .expect("clean staged Markdown diff should include document content");
    assert!(clean_staged_document.can_edit);
    assert_eq!(clean_staged_document.edit_blocked_reason, None);

    fs::write(&markdown_file, "# Worktree\n\nNot staged yet.\n").unwrap();

    let staged = load_git_diff_for_request(
        &repo_root,
        &GitDiffRequest {
            original_path: None,
            path: "README.md".to_owned(),
            section_id: GitDiffSection::Staged,
            status_code: Some("M".to_owned()),
            workdir: repo_root.to_string_lossy().into_owned(),
            project_id: None,
            session_id: None,
        },
    )
    .unwrap();
    let staged_document = staged
        .document_content
        .expect("staged Markdown diff should include document content");
    assert_eq!(
        staged_document.before.source,
        GitDiffDocumentSideSource::Head
    );
    assert_eq!(
        staged_document.after.source,
        GitDiffDocumentSideSource::Index
    );
    assert!(!staged_document.can_edit);
    assert_eq!(
        staged_document.edit_blocked_reason.as_deref(),
        Some(
            "This staged Markdown diff is read-only because the worktree has unstaged changes for this file."
        )
    );
    assert_eq!(staged_document.before.content, "# Base\n\nInitial text.\n");
    assert_eq!(
        staged_document.after.content,
        "# Staged\n\nReady to commit.\n"
    );

    let unstaged = load_git_diff_for_request(
        &repo_root,
        &GitDiffRequest {
            original_path: None,
            path: "README.md".to_owned(),
            section_id: GitDiffSection::Unstaged,
            status_code: Some("M".to_owned()),
            workdir: repo_root.to_string_lossy().into_owned(),
            project_id: None,
            session_id: None,
        },
    )
    .unwrap();
    let unstaged_document = unstaged
        .document_content
        .expect("unstaged Markdown diff should include document content");
    assert_eq!(
        unstaged_document.before.source,
        GitDiffDocumentSideSource::Index
    );
    assert_eq!(
        unstaged_document.after.source,
        GitDiffDocumentSideSource::Worktree
    );
    assert!(unstaged_document.can_edit);
    assert_eq!(unstaged_document.edit_blocked_reason, None);
    assert_eq!(
        unstaged_document.before.content,
        "# Staged\n\nReady to commit.\n"
    );
    assert_eq!(
        unstaged_document.after.content,
        "# Worktree\n\nNot staged yet.\n"
    );

    fs::remove_dir_all(repo_root).unwrap();
}

// Pins enrichment sides for added/deleted/untracked Markdown paths and
// confirms non-Markdown files skip enrichment entirely without a note.
// Guards against empty-source misclassification (e.g. showing HEAD
// content on an added file) and against silently enriching `.txt`.
#[test]
fn git_diff_document_content_covers_added_deleted_untracked_and_non_markdown() {
    let repo_root =
        std::env::temp_dir().join(format!("termal-git-diff-doc-status-{}", Uuid::new_v4()));
    let tracked_file = repo_root.join("tracked.md");
    let deleted_staged_file = repo_root.join("deleted-staged.md");
    let deleted_unstaged_file = repo_root.join("deleted-unstaged.md");
    let added_staged_file = repo_root.join("added-staged.md");
    let untracked_file = repo_root.join("untracked.md");
    let text_file = repo_root.join("notes.txt");

    fs::create_dir_all(&repo_root).unwrap();
    fs::write(&tracked_file, "# Tracked\n").unwrap();
    fs::write(&deleted_staged_file, "# Delete staged\n").unwrap();
    fs::write(&deleted_unstaged_file, "# Delete unstaged\n").unwrap();
    fs::write(&text_file, "plain text\n").unwrap();
    init_git_document_test_repo(&repo_root);
    run_git_test_command(&repo_root, &["add", "."]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);

    fs::write(&added_staged_file, "# Added staged\n").unwrap();
    run_git_test_command(&repo_root, &["add", "added-staged.md"]);
    run_git_test_command(&repo_root, &["rm", "deleted-staged.md"]);
    fs::remove_file(&deleted_unstaged_file).unwrap();
    fs::write(&untracked_file, "# Untracked\n").unwrap();
    fs::write(&text_file, "plain text changed\n").unwrap();

    let staged_added = load_git_diff_for_request(
        &repo_root,
        &GitDiffRequest {
            original_path: None,
            path: "added-staged.md".to_owned(),
            section_id: GitDiffSection::Staged,
            status_code: Some("A".to_owned()),
            workdir: repo_root.to_string_lossy().into_owned(),
            project_id: None,
            session_id: None,
        },
    )
    .unwrap()
    .document_content
    .expect("staged added Markdown should include document content");
    assert_eq!(staged_added.before.source, GitDiffDocumentSideSource::Empty);
    assert_eq!(staged_added.before.content, "");
    assert_eq!(staged_added.after.source, GitDiffDocumentSideSource::Index);
    assert_eq!(staged_added.after.content, "# Added staged\n");

    let unstaged_untracked = load_git_diff_for_request(
        &repo_root,
        &GitDiffRequest {
            original_path: None,
            path: "untracked.md".to_owned(),
            section_id: GitDiffSection::Unstaged,
            status_code: Some("?".to_owned()),
            workdir: repo_root.to_string_lossy().into_owned(),
            project_id: None,
            session_id: None,
        },
    )
    .unwrap()
    .document_content
    .expect("unstaged untracked Markdown should include document content");
    assert_eq!(
        unstaged_untracked.before.source,
        GitDiffDocumentSideSource::Empty
    );
    assert_eq!(unstaged_untracked.before.content, "");
    assert_eq!(
        unstaged_untracked.after.source,
        GitDiffDocumentSideSource::Worktree
    );
    assert_eq!(unstaged_untracked.after.content, "# Untracked\n");

    let staged_deleted = load_git_diff_for_request(
        &repo_root,
        &GitDiffRequest {
            original_path: None,
            path: "deleted-staged.md".to_owned(),
            section_id: GitDiffSection::Staged,
            status_code: Some("D".to_owned()),
            workdir: repo_root.to_string_lossy().into_owned(),
            project_id: None,
            session_id: None,
        },
    )
    .unwrap()
    .document_content
    .expect("staged deleted Markdown should include document content");
    assert_eq!(
        staged_deleted.before.source,
        GitDiffDocumentSideSource::Head
    );
    assert_eq!(staged_deleted.before.content, "# Delete staged\n");
    assert_eq!(
        staged_deleted.after.source,
        GitDiffDocumentSideSource::Empty
    );
    assert_eq!(staged_deleted.after.content, "");

    let unstaged_deleted = load_git_diff_for_request(
        &repo_root,
        &GitDiffRequest {
            original_path: None,
            path: "deleted-unstaged.md".to_owned(),
            section_id: GitDiffSection::Unstaged,
            status_code: Some("D".to_owned()),
            workdir: repo_root.to_string_lossy().into_owned(),
            project_id: None,
            session_id: None,
        },
    )
    .unwrap()
    .document_content
    .expect("unstaged deleted Markdown should include document content");
    assert_eq!(
        unstaged_deleted.before.source,
        GitDiffDocumentSideSource::Index
    );
    assert_eq!(unstaged_deleted.before.content, "# Delete unstaged\n");
    assert_eq!(
        unstaged_deleted.after.source,
        GitDiffDocumentSideSource::Empty
    );
    assert_eq!(unstaged_deleted.after.content, "");

    let non_markdown = load_git_diff_for_request(
        &repo_root,
        &GitDiffRequest {
            original_path: None,
            path: "notes.txt".to_owned(),
            section_id: GitDiffSection::Unstaged,
            status_code: Some("M".to_owned()),
            workdir: repo_root.to_string_lossy().into_owned(),
            project_id: None,
            session_id: None,
        },
    )
    .unwrap();
    assert!(non_markdown.document_content.is_none());
    assert_eq!(non_markdown.document_enrichment_note, None);

    fs::remove_dir_all(repo_root).unwrap();
}

// Pins that an unstaged edit on top of a staged rename reads the
// index side at the new path, not the old pre-rename path. Guards
// against a regression where `original_path` would override the index
// lookup and surface stale HEAD text as the "before" side.
#[test]
fn git_diff_document_content_uses_current_index_path_for_unstaged_staged_rename() {
    let repo_root =
        std::env::temp_dir().join(format!("termal-git-diff-doc-rename-{}", Uuid::new_v4()));
    let old_file = repo_root.join("old.md");
    let new_file = repo_root.join("new.md");

    fs::create_dir_all(&repo_root).unwrap();
    fs::write(&old_file, "# Old\n\nBase text.\n").unwrap();

    run_git_test_command(&repo_root, &["init"]);
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);
    run_git_test_command(&repo_root, &["add", "old.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);

    fs::rename(&old_file, &new_file).unwrap();
    fs::write(&new_file, "# New\n\nStaged rename text.\n").unwrap();
    run_git_test_command(&repo_root, &["add", "-A"]);
    fs::write(&new_file, "# New\n\nWorktree edit text.\n").unwrap();

    let response = load_git_diff_for_request(
        &repo_root,
        &GitDiffRequest {
            original_path: Some("old.md".to_owned()),
            path: "new.md".to_owned(),
            section_id: GitDiffSection::Unstaged,
            status_code: Some("M".to_owned()),
            workdir: repo_root.to_string_lossy().into_owned(),
            project_id: None,
            session_id: None,
        },
    )
    .unwrap();
    let document = response
        .document_content
        .expect("unstaged Markdown edit should include document content");

    assert_eq!(document.before.source, GitDiffDocumentSideSource::Index);
    assert_eq!(document.before.content, "# New\n\nStaged rename text.\n");
    assert_eq!(document.after.source, GitDiffDocumentSideSource::Worktree);
    assert_eq!(document.after.content, "# New\n\nWorktree edit text.\n");

    fs::remove_dir_all(repo_root).unwrap();
}

// Pins that non-UTF-8 bytes in a Markdown worktree file degrade the
// response to raw patch plus the UTF-8 enrichment note rather than
// surfacing an error to the user. Guards against byte-string panics
// and keeps the rendered preview gracefully unavailable.
#[test]
fn git_diff_document_content_skips_non_utf8_markdown() {
    let repo_root =
        std::env::temp_dir().join(format!("termal-git-diff-doc-non-utf8-{}", Uuid::new_v4()));
    let markdown_file = repo_root.join("README.md");

    fs::create_dir_all(&repo_root).unwrap();
    fs::write(&markdown_file, "# Base\n\nPlain text.\n").unwrap();

    run_git_test_command(&repo_root, &["init"]);
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);
    run_git_test_command(&repo_root, &["add", "README.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);

    fs::write(&markdown_file, b"# Base\n\nLatin-1: \xE9\n").unwrap();

    let response = load_git_diff_for_request(
        &repo_root,
        &GitDiffRequest {
            original_path: None,
            path: "README.md".to_owned(),
            section_id: GitDiffSection::Unstaged,
            status_code: Some("M".to_owned()),
            workdir: repo_root.to_string_lossy().into_owned(),
            project_id: None,
            session_id: None,
        },
    )
    .unwrap();

    assert!(response.document_content.is_none());
    assert_eq!(
        response.document_enrichment_note.as_deref(),
        Some("Rendered Markdown is unavailable because the document is not valid UTF-8.")
    );

    fs::remove_dir_all(repo_root).unwrap();
}

// Pins that `read_git_worktree_text` rejects files past
// `MAX_FILE_CONTENT_BYTES` with a BAD_REQUEST and a
// `GitDocumentTooLarge`-keyed note. Guards against OOM when a user
// opens a diff on a multi-gigabyte file and ensures the 10 MB limit
// is surfaced verbatim to the UI.
#[test]
fn git_diff_document_readers_reject_oversized_worktree_files() {
    let repo_root =
        std::env::temp_dir().join(format!("termal-git-diff-doc-large-{}", Uuid::new_v4()));
    let markdown_file = repo_root.join("README.md");

    fs::create_dir_all(&repo_root).unwrap();
    fs::write(&markdown_file, "a".repeat(MAX_FILE_CONTENT_BYTES + 1)).unwrap();

    let error = read_git_worktree_text(&repo_root, "README.md")
        .expect_err("oversized worktree document should be rejected");

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("read limit"));
    assert_eq!(
        git_diff_document_enrichment_note(&error).as_deref(),
        Some("Rendered Markdown is unavailable because the document exceeds the 10 MB read limit.")
    );

    fs::remove_dir_all(repo_root).unwrap();
}

// Pins that `git_diff_document_enrichment_note` dispatches on
// `ApiErrorKind`, not error message substrings. Guards against a
// future contributor rewording "10 MB read limit" and silently
// breaking the user-visible note, and preserves the legacy
// untagged-status fallback for errors without a kind.
#[test]
fn git_diff_document_enrichment_note_uses_structured_error_kind() {
    let untagged_error = ApiError::bad_request("git worktree file exceeds the 10 MB read limit");
    assert_eq!(
        git_diff_document_enrichment_note(&untagged_error).as_deref(),
        Some("Rendered Markdown is unavailable.")
    );
    let untagged_not_found_error = ApiError::not_found("git worktree file not found");
    assert_eq!(
        git_diff_document_enrichment_note(&untagged_not_found_error).as_deref(),
        Some("Rendered Markdown is unavailable.")
    );

    let tagged_error = ApiError::bad_request("read ceiling wording changed")
        .with_kind(ApiErrorKind::GitDocumentTooLarge);
    assert_eq!(
        git_diff_document_enrichment_note(&tagged_error).as_deref(),
        Some("Rendered Markdown is unavailable because the document exceeds the 10 MB read limit.")
    );
}

#[test]
fn git_diff_document_enrichment_note_suppresses_session_lookup_recovery_kinds() {
    for kind in [
        ApiErrorKind::LocalSessionMissing,
        ApiErrorKind::RemoteConnectionUnavailable,
        ApiErrorKind::RemoteSessionHydrationFreshnessRace,
        ApiErrorKind::RemoteSessionMissingFullTranscript,
    ] {
        let error = ApiError::bad_gateway("recoverable remote hydration").with_kind(kind);
        assert_eq!(git_diff_document_enrichment_note(&error), None);
    }
}

// Pins that every status in `DEGRADED_UNTAGGED_STATUSES` both
// triggers degradation and produces a user-visible note, keeping the
// two helpers in lockstep. Guards against adding a status to the
// degrade list while forgetting to also teach the note helper, which
// would ship a blank-explanation raw diff.
#[test]
fn git_diff_degraded_untagged_statuses_always_produce_a_note() {
    for status in DEGRADED_UNTAGGED_STATUSES {
        let error = ApiError {
            status: *status,
            message: format!("untagged {status} error"),
            kind: None,
        };
        assert!(
            should_degrade_git_diff_document_enrichment_error(&error),
            "status {status} must be treated as degradable",
        );
        assert!(
            git_diff_document_enrichment_note(&error).is_some(),
            "status {status} must produce a user-visible enrichment note",
        );
    }
}

// Pins end-to-end degradation: an oversized worktree Markdown file
// returns the raw patch, drops `document_content`, and attaches the
// 10 MB enrichment note. Guards against the response path swallowing
// the oversize error into an opaque 500 or returning partial document
// content.
#[test]
fn git_diff_response_reports_oversized_markdown_enrichment_note() {
    let repo_root = std::env::temp_dir().join(format!(
        "termal-git-diff-doc-large-response-{}",
        Uuid::new_v4()
    ));
    let markdown_file = repo_root.join("README.md");

    fs::create_dir_all(&repo_root).unwrap();
    fs::write(&markdown_file, "# Base\n").unwrap();
    init_git_document_test_repo(&repo_root);
    run_git_test_command(&repo_root, &["add", "README.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);
    fs::write(
        &markdown_file,
        format!("{}\n", "a".repeat(MAX_FILE_CONTENT_BYTES + 1)),
    )
    .unwrap();

    let response = load_git_diff_for_request(
        &repo_root,
        &GitDiffRequest {
            original_path: None,
            path: "README.md".to_owned(),
            section_id: GitDiffSection::Unstaged,
            status_code: Some("M".to_owned()),
            workdir: repo_root.to_string_lossy().into_owned(),
            project_id: None,
            session_id: None,
        },
    )
    .expect("oversized Markdown enrichment should return the raw diff");

    assert!(response.diff.contains("-# Base"));
    assert!(response.diff.contains("+"));
    assert!(response.document_content.is_none());
    assert_eq!(
        response.document_enrichment_note.as_deref(),
        Some("Rendered Markdown is unavailable because the document exceeds the 10 MB read limit.")
    );

    fs::remove_dir_all(repo_root).unwrap();
}

// Pins that an unexpected `ApiError::internal` from the document
// loader still returns the already-loaded raw patch with a generic
// "read error" note rather than failing the whole request. Guards
// against a transient disk error hiding the available diff from the
// user.
#[test]
fn git_diff_response_degrades_internal_markdown_enrichment_errors_to_raw_diff() {
    let repo_root = std::env::temp_dir().join(format!(
        "termal-git-diff-doc-internal-response-{}",
        Uuid::new_v4()
    ));
    let markdown_file = repo_root.join("README.md");

    fs::create_dir_all(&repo_root).unwrap();
    fs::write(&markdown_file, "# Base\n").unwrap();
    init_git_document_test_repo(&repo_root);
    run_git_test_command(&repo_root, &["add", "README.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);
    fs::write(&markdown_file, "# Changed\n").unwrap();

    let response = load_git_diff_for_request_with_document_loader(
        &repo_root,
        &GitDiffRequest {
            original_path: None,
            path: "README.md".to_owned(),
            section_id: GitDiffSection::Unstaged,
            status_code: Some("M".to_owned()),
            workdir: repo_root.to_string_lossy().into_owned(),
            project_id: None,
            session_id: None,
        },
        |_repo_root, _current_path, _original_path, _status_code, _section_id| {
            Err(ApiError::internal(
                "failed to read git worktree file: disk error",
            ))
        },
    )
    .expect("internal Markdown enrichment errors should return the raw diff");

    assert!(response.diff.contains("-# Base"));
    assert!(response.diff.contains("+# Changed"));
    assert!(response.document_content.is_none());
    assert_eq!(
        response.document_enrichment_note.as_deref(),
        Some("Rendered Markdown is unavailable due to a read error.")
    );

    fs::remove_dir_all(repo_root).unwrap();
}

// Pins the exact user-facing note for each expected enrichment
// failure kind (became-symlink, invalid-utf8, not-file, not-found,
// too-large). Guards against rewording drift between the production
// note text and the UI contract, keeping the five message templates
// stable.
#[test]
fn git_diff_response_reports_expected_document_enrichment_notes() {
    let repo_root = std::env::temp_dir().join(format!(
        "termal-git-diff-doc-notes-response-{}",
        Uuid::new_v4()
    ));
    let markdown_file = repo_root.join("README.md");

    fs::create_dir_all(&repo_root).unwrap();
    fs::write(&markdown_file, "# Base\n").unwrap();
    init_git_document_test_repo(&repo_root);
    run_git_test_command(&repo_root, &["add", "README.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);
    fs::write(&markdown_file, "# Changed\n").unwrap();

    for (kind, expected_note) in [
        (
            ApiErrorKind::GitDocumentBecameSymlink,
            "Rendered Markdown is unavailable because the file changed to a symlink while loading.",
        ),
        (
            ApiErrorKind::GitDocumentInvalidUtf8,
            "Rendered Markdown is unavailable because the document is not valid UTF-8.",
        ),
        (
            ApiErrorKind::GitDocumentNotFile,
            "Rendered Markdown is unavailable because the path is not a regular file.",
        ),
        (
            ApiErrorKind::GitDocumentNotFound,
            "Rendered Markdown is unavailable because the document could not be found.",
        ),
        (
            ApiErrorKind::GitDocumentTooLarge,
            "Rendered Markdown is unavailable because the document exceeds the 10 MB read limit.",
        ),
    ] {
        let response = load_git_diff_for_request_with_document_loader(
            &repo_root,
            &GitDiffRequest {
                original_path: None,
                path: "README.md".to_owned(),
                section_id: GitDiffSection::Unstaged,
                status_code: Some("M".to_owned()),
                workdir: repo_root.to_string_lossy().into_owned(),
                project_id: None,
                session_id: None,
            },
            move |_repo_root, _current_path, _original_path, _status_code, _section_id| {
                Err(ApiError::bad_request("document enrichment unavailable").with_kind(kind))
            },
        )
        .expect("expected Markdown enrichment errors should return the raw diff");

        assert!(response.diff.contains("-# Base"));
        assert!(response.diff.contains("+# Changed"));
        assert!(response.document_content.is_none());
        assert_eq!(
            response.document_enrichment_note.as_deref(),
            Some(expected_note)
        );
    }

    fs::remove_dir_all(repo_root).unwrap();
}

// Pins the camelCase JSON wire names (`documentEnrichmentNote`,
// `documentContent`) and their absence-behaviour when enrichment
// degrades. Guards against serde rename drift leaking
// `document_enrichment_note` snake_case or serializing a null
// `documentContent` that the frontend does not expect.
#[test]
fn git_diff_response_degraded_markdown_serializes_frontend_contract() {
    let response = GitDiffResponse {
        change_type: GitDiffChangeType::Edit,
        change_set_id: "git-diff-test".to_owned(),
        diff: "-# Base\n+# Changed\n".to_owned(),
        diff_id: "git:test".to_owned(),
        file_path: Some("/repo/README.md".to_owned()),
        language: Some("markdown".to_owned()),
        document_enrichment_note: Some(
            "Rendered Markdown is unavailable because the document is not valid UTF-8.".to_owned(),
        ),
        document_content: None,
        summary: "Unstaged changes in README.md".to_owned(),
    };

    let value = serde_json::to_value(response).unwrap();

    assert_eq!(
        value.get("documentEnrichmentNote").and_then(Value::as_str),
        Some("Rendered Markdown is unavailable because the document is not valid UTF-8.")
    );
    assert!(value.get("documentContent").is_none());
    assert!(value.get("document_enrichment_note").is_none());
    assert!(value.get("document_content").is_none());
}

// Pins that HEAD/index/worktree reader errors always carry non-empty
// messages mentioning "git". Guards against blank or generic errors
// that would leave the UI with no diagnostic context when a document
// side fails to load.
#[test]
fn git_diff_document_reader_errors_are_non_empty() {
    let repo_root =
        std::env::temp_dir().join(format!("termal-git-diff-doc-errors-{}", Uuid::new_v4()));

    fs::create_dir_all(&repo_root).unwrap();
    run_git_test_command(&repo_root, &["init"]);

    for error in [
        read_git_object_text(&repo_root, "HEAD", "missing.md").unwrap_err(),
        read_git_index_text(&repo_root, "missing.md").unwrap_err(),
        read_git_worktree_text(&repo_root, "missing.md").unwrap_err(),
    ] {
        assert!(!error.message.trim().is_empty());
        assert!(
            error.message.to_lowercase().contains("git"),
            "error should mention git: {}",
            error.message
        );
    }

    fs::remove_dir_all(repo_root).unwrap();
}

// Pins a missing blob at `HEAD:missing.md` mapping to
// `StatusCode::NOT_FOUND` with the "could not be found" enrichment
// note. Guards against misreporting a clean repo lookup failure as a
// 500 or a bad-request.
#[test]
fn git_diff_document_reader_reports_missing_git_objects_as_not_found() {
    let repo_root = std::env::temp_dir().join(format!(
        "termal-git-diff-doc-missing-object-{}",
        Uuid::new_v4()
    ));
    let markdown_file = repo_root.join("README.md");

    fs::create_dir_all(&repo_root).unwrap();
    fs::write(&markdown_file, "# Base\n").unwrap();
    run_git_test_command(&repo_root, &["init"]);
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);
    run_git_test_command(&repo_root, &["add", "README.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);

    let error = read_git_object_text(&repo_root, "HEAD", "missing.md")
        .expect_err("missing git object should fail");

    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert!(error.message.contains("git object not found"));
    assert_eq!(
        git_diff_document_enrichment_note(&error).as_deref(),
        Some("Rendered Markdown is unavailable because the document could not be found.")
    );

    fs::remove_dir_all(repo_root).unwrap();
}

// Pins that a freshly-initialised repo (unborn HEAD) returns
// `NOT_FOUND` instead of the more exotic "ambiguous argument" wording
// git emits. Guards against the UI surfacing confusing low-level git
// diagnostics when diffing in a repo that has no commits yet.
#[test]
fn git_diff_document_reader_reports_unborn_head_objects_as_not_found() {
    let repo_root = std::env::temp_dir().join(format!(
        "termal-git-diff-doc-unborn-head-{}",
        Uuid::new_v4()
    ));

    fs::create_dir_all(&repo_root).unwrap();
    run_git_test_command(&repo_root, &["init"]);

    let error = read_git_object_text(&repo_root, "HEAD", "missing.md")
        .expect_err("unborn HEAD git object should fail");

    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert!(error.message.contains("git object not found"));
    assert_eq!(
        git_diff_document_enrichment_note(&error).as_deref(),
        Some("Rendered Markdown is unavailable because the document could not be found.")
    );

    fs::remove_dir_all(repo_root).unwrap();
}

// Pins that `read_git_object_text` streams the `git cat-file` output
// and aborts at the read ceiling instead of buffering the full blob.
// Guards against memory blow-up when HEAD contains a very large
// committed file and keeps the bad-request envelope consistent with
// worktree oversize.
#[test]
fn git_diff_document_reader_rejects_oversized_git_objects() {
    let repo_root = std::env::temp_dir().join(format!(
        "termal-git-diff-doc-large-object-{}",
        Uuid::new_v4()
    ));
    let markdown_file = repo_root.join("README.md");

    fs::create_dir_all(&repo_root).unwrap();
    fs::write(&markdown_file, "a".repeat(MAX_FILE_CONTENT_BYTES + 1)).unwrap();
    run_git_test_command(&repo_root, &["init"]);
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);
    run_git_test_command(&repo_root, &["add", "README.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "large"]);

    let error = read_git_object_text(&repo_root, "HEAD", "README.md")
        .expect_err("oversized git object should fail");

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("read limit"));

    fs::remove_dir_all(repo_root).unwrap();
}

// Pins that a directory at a worktree path maps to `NOT_FOUND` with
// the "not a regular file" enrichment note. Guards against reading a
// directory handle as bytes and classifies a stale diff request
// against a now-directory entry the same way as a missing file.
#[test]
fn git_diff_worktree_reader_reports_directories_as_not_found() {
    let repo_root =
        std::env::temp_dir().join(format!("termal-git-diff-doc-directory-{}", Uuid::new_v4()));
    let docs_dir = repo_root.join("docs");

    fs::create_dir_all(&docs_dir).unwrap();

    let error = read_git_worktree_text(&repo_root, "docs")
        .expect_err("directory worktree entry should not be read as a file");

    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert!(error.message.contains("git worktree path is not a file"));
    assert_eq!(
        git_diff_document_enrichment_note(&error).as_deref(),
        Some("Rendered Markdown is unavailable because the path is not a regular file.")
    );

    fs::remove_dir_all(repo_root).unwrap();
}

// Pins that a missing worktree path returns `NOT_FOUND` with the
// relative name but without leaking the absolute repo root.
// Guards against path-disclosure in error messages and keeps the
// enrichment note stable at "could not be found".
#[test]
fn git_diff_worktree_reader_reports_missing_files_as_not_found() {
    let repo_root = std::env::temp_dir().join(format!(
        "termal-git-diff-doc-missing-worktree-{}",
        Uuid::new_v4()
    ));

    fs::create_dir_all(&repo_root).unwrap();

    let error = read_git_worktree_text(&repo_root, "missing.md")
        .expect_err("missing worktree file should fail");

    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert!(
        error
            .message
            .contains("git worktree path not found: missing.md")
    );
    assert!(
        !error.message.contains(repo_root.to_string_lossy().as_ref()),
        "error should not expose absolute repo path: {}",
        error.message
    );
    assert_eq!(
        git_diff_document_enrichment_note(&error).as_deref(),
        Some("Rendered Markdown is unavailable because the document could not be found.")
    );

    fs::remove_dir_all(repo_root).unwrap();
}

// Pins that the untracked-file diff builder enforces
// `MAX_FILE_CONTENT_BYTES` before synthesising a full-add patch.
// Guards against building a multi-megabyte patch string for a huge
// new file that would never render usefully and keeps the bad-request
// wording consistent with the other readers.
#[test]
fn git_diff_untracked_reader_rejects_oversized_files() {
    let repo_root = std::env::temp_dir().join(format!(
        "termal-git-diff-untracked-large-{}",
        Uuid::new_v4()
    ));
    let markdown_file = repo_root.join("README.md");

    fs::create_dir_all(&repo_root).unwrap();
    fs::write(&markdown_file, "a".repeat(MAX_FILE_CONTENT_BYTES + 1)).unwrap();

    let error = build_untracked_git_diff(&repo_root, "README.md")
        .expect_err("oversized untracked diff should fail");

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("read limit"));

    fs::remove_dir_all(repo_root).unwrap();
}

// --- Unix-only symlink safety: five tests pinning the invariant that
// worktree reads must canonicalize through symlinks without letting
// either a symlink leaf or a symlinked parent escape the repo root.
// Skipped on Windows because the platform's symlink semantics and
// permission requirements differ; the security contract only applies
// where TermAl may encounter POSIX-style symlinks on disk.

// Pins that a symlink pointing at a target file inside the same repo
// is followed and the target contents are returned. Guards against an
// over-aggressive symlink rejection that would break legitimate
// in-repo symlinked Markdown.
#[cfg(unix)]
#[test]
fn git_diff_worktree_reader_returns_symlink_target_file_contents() {
    let repo_root =
        std::env::temp_dir().join(format!("termal-git-diff-doc-symlink-{}", Uuid::new_v4()));
    let target_file = repo_root.join("target.md");
    let symlink_path = repo_root.join("link.md");

    fs::create_dir_all(&repo_root).unwrap();
    fs::write(&target_file, "# Target\n").unwrap();
    std::os::unix::fs::symlink(&target_file, &symlink_path).unwrap();

    let content = read_git_worktree_text(&repo_root, "link.md").unwrap();

    assert_eq!(content, "# Target\n");

    fs::remove_dir_all(repo_root).unwrap();
}

// Pins that a symlink whose target resolves outside the repo root
// returns `BAD_REQUEST` with "escapes repository root" and does not
// leak the absolute repo path. Guards against the classic
// read-arbitrary-file attack via a crafted in-repo symlink.
#[cfg(unix)]
#[test]
fn git_diff_worktree_reader_rejects_symlink_target_escape() {
    let repo_root = std::env::temp_dir().join(format!(
        "termal-git-diff-doc-symlink-escape-{}",
        Uuid::new_v4()
    ));
    let outside_root = std::env::temp_dir().join(format!(
        "termal-git-diff-doc-symlink-outside-{}",
        Uuid::new_v4()
    ));
    let outside_file = outside_root.join("secret.md");
    let symlink_path = repo_root.join("link.md");

    fs::create_dir_all(&repo_root).unwrap();
    fs::create_dir_all(&outside_root).unwrap();
    fs::write(&outside_file, "# Secret\n").unwrap();
    std::os::unix::fs::symlink(&outside_file, &symlink_path).unwrap();

    let error = read_git_worktree_text(&repo_root, "link.md")
        .expect_err("symlink target outside repo should fail");

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("escapes repository root"));
    assert!(
        !error.message.contains(repo_root.to_string_lossy().as_ref()),
        "error should not expose absolute repo path: {}",
        error.message
    );

    fs::remove_dir_all(repo_root).unwrap();
    fs::remove_dir_all(outside_root).unwrap();
}

// Pins that a symlinked directory in a path component also blocks
// escape, even when the leaf is a plain file outside the repo.
// Guards against walking through a symlinked parent without
// re-checking containment at each level.
#[cfg(unix)]
#[test]
fn git_diff_worktree_reader_rejects_symlinked_parent_escape() {
    let repo_root = std::env::temp_dir().join(format!(
        "termal-git-diff-doc-parent-symlink-{}",
        Uuid::new_v4()
    ));
    let outside_root = std::env::temp_dir().join(format!(
        "termal-git-diff-doc-parent-target-{}",
        Uuid::new_v4()
    ));
    let outside_file = outside_root.join("secret.md");
    let symlink_dir = repo_root.join("linked");

    fs::create_dir_all(&repo_root).unwrap();
    fs::create_dir_all(&outside_root).unwrap();
    fs::write(&outside_file, "# Secret\n").unwrap();
    std::os::unix::fs::symlink(&outside_root, &symlink_dir).unwrap();

    let error = read_git_worktree_text(&repo_root, "linked/secret.md")
        .expect_err("symlinked parent should not escape the repo");

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("escapes repository root"));

    fs::remove_dir_all(repo_root).unwrap();
    fs::remove_dir_all(outside_root).unwrap();
}

// Pins that a symlinked leaf inside a symlinked parent is rejected
// at the parent check, not accidentally allowed because the leaf
// itself resolves "somewhere inside `outside_root`". Guards against
// TOCTOU-adjacent mistakes where two independent symlinks combine to
// defeat containment.
#[cfg(unix)]
#[test]
fn git_diff_worktree_reader_rejects_symlinked_parent_symlink_leaf_escape() {
    let repo_root = std::env::temp_dir().join(format!(
        "termal-git-diff-doc-parent-leaf-symlink-{}",
        Uuid::new_v4()
    ));
    let outside_root = std::env::temp_dir().join(format!(
        "termal-git-diff-doc-parent-leaf-target-{}",
        Uuid::new_v4()
    ));
    let outside_target = outside_root.join("target.md");
    let outside_link = outside_root.join("linked-leaf.md");
    let symlink_dir = repo_root.join("linked");

    fs::create_dir_all(&repo_root).unwrap();
    fs::create_dir_all(&outside_root).unwrap();
    fs::write(&outside_target, "# Secret\n").unwrap();
    std::os::unix::fs::symlink(&outside_target, &outside_link).unwrap();
    std::os::unix::fs::symlink(&outside_root, &symlink_dir).unwrap();

    let error = read_git_worktree_text(&repo_root, "linked/linked-leaf.md")
        .expect_err("symlinked parent should be rejected before reading symlink leaf");

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("escapes repository root"));

    fs::remove_dir_all(repo_root).unwrap();
    fs::remove_dir_all(outside_root).unwrap();
}

// Pins that a non-UTF-8 symlink target path is still followed when
// the target's bytes are valid UTF-8 text. Guards against an
// over-strict OsStr → str conversion on the intermediate target path
// refusing legitimate POSIX filesystems with non-UTF-8 filenames.
#[cfg(unix)]
#[test]
fn git_diff_worktree_reader_allows_non_utf8_symlink_targets() {
    use std::os::unix::ffi::OsStringExt as _;

    let repo_root = std::env::temp_dir().join(format!(
        "termal-git-diff-doc-non-utf8-symlink-{}",
        Uuid::new_v4()
    ));
    let target_name = std::ffi::OsString::from_vec(vec![
        b't', b'a', 0xFF, b'r', b'g', b'e', b't', b'.', b'm', b'd',
    ]);
    let target_path = repo_root.join(FsPath::new(&target_name));
    let symlink_path = repo_root.join("link.md");

    fs::create_dir_all(&repo_root).unwrap();
    fs::write(&target_path, "# Target\n").unwrap();
    std::os::unix::fs::symlink(&target_path, &symlink_path).unwrap();

    let content = read_git_worktree_text(&repo_root, "link.md").unwrap();

    assert_eq!(content, "# Target\n");

    fs::remove_dir_all(repo_root).unwrap();
}

// Pins that `push_git_repo` advances the tracking branch so the
// remote HEAD matches the local HEAD and the response reports
// ahead=0/behind=0 with a "Pushed " summary. Guards against reporting
// success while the remote is still behind.
#[test]
fn push_git_repo_updates_tracking_branch() {
    let root = std::env::temp_dir().join(format!("termal-git-push-{}", Uuid::new_v4()));
    let remote_root = root.join("remote.git");
    let repo_root = root.join("local");
    let remote_root_string = remote_root.to_string_lossy().into_owned();
    let repo_root_string = repo_root.to_string_lossy().into_owned();

    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&remote_root).unwrap();

    run_git_test_command(&remote_root, &["init", "--bare"]);
    run_git_test_command(
        &root,
        &[
            "clone",
            remote_root_string.as_str(),
            repo_root_string.as_str(),
        ],
    );
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);

    fs::write(repo_root.join("README.md"), "# Init\n").unwrap();
    run_git_test_command(&repo_root, &["add", "README.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);
    run_git_test_command(&repo_root, &["push", "-u", "origin", "HEAD"]);

    fs::write(repo_root.join("README.md"), "# Updated\n").unwrap();
    run_git_test_command(&repo_root, &["commit", "-am", "update"]);

    let response = push_git_repo(&repo_root).unwrap();
    let local_head = run_git_test_command_output(&repo_root, &["rev-parse", "HEAD"]);
    let remote_head = run_git_test_command_output(&remote_root, &["rev-parse", "HEAD"]);

    assert_eq!(local_head, remote_head);
    assert_eq!(response.status.ahead, 0);
    assert_eq!(response.status.behind, 0);
    assert!(response.summary.starts_with("Pushed "));

    fs::remove_dir_all(root).unwrap();
}

// Pins that `sync_git_repo` fast-forwards the local branch to match
// a peer's pushed commit, updating both HEAD and the README content,
// and reports ahead=0/behind=0 with a "Synced " summary. Guards
// against a pull that reports success while leaving the worktree
// unchanged.
#[test]
fn sync_git_repo_pulls_remote_changes() {
    let root = std::env::temp_dir().join(format!("termal-git-sync-{}", Uuid::new_v4()));
    let remote_root = root.join("remote.git");
    let repo_root = root.join("local");
    let peer_root = root.join("peer");
    let remote_root_string = remote_root.to_string_lossy().into_owned();
    let repo_root_string = repo_root.to_string_lossy().into_owned();
    let peer_root_string = peer_root.to_string_lossy().into_owned();

    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&remote_root).unwrap();

    run_git_test_command(&remote_root, &["init", "--bare"]);
    run_git_test_command(
        &root,
        &[
            "clone",
            remote_root_string.as_str(),
            repo_root_string.as_str(),
        ],
    );
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);

    fs::write(repo_root.join("README.md"), "# Init\n").unwrap();
    run_git_test_command(&repo_root, &["add", "README.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);
    run_git_test_command(&repo_root, &["push", "-u", "origin", "HEAD"]);

    run_git_test_command(
        &root,
        &[
            "clone",
            remote_root_string.as_str(),
            peer_root_string.as_str(),
        ],
    );
    run_git_test_command(&peer_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&peer_root, &["config", "user.name", "TermAl"]);
    fs::write(peer_root.join("README.md"), "# Peer\n").unwrap();
    run_git_test_command(&peer_root, &["commit", "-am", "peer update"]);
    run_git_test_command(&peer_root, &["push"]);

    let response = sync_git_repo(&repo_root).unwrap();
    let local_head = run_git_test_command_output(&repo_root, &["rev-parse", "HEAD"]);
    let remote_head = run_git_test_command_output(&remote_root, &["rev-parse", "HEAD"]);

    assert_eq!(
        fs::read_to_string(repo_root.join("README.md"))
            .unwrap()
            .replace("\r\n", "\n"),
        "# Peer\n",
    );
    assert_eq!(local_head, remote_head);
    assert_eq!(response.status.ahead, 0);
    assert_eq!(response.status.behind, 0);
    assert!(response.summary.starts_with("Synced "));

    fs::remove_dir_all(root).unwrap();
}
