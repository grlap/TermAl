// Git HTTP wire types — requests, responses, and file-level status shapes.
//
// Groups all DTOs consumed or returned by `api_git.rs` routes. Split
// out of wire.rs to keep the shared type vocabulary navigable.

// Git requests

/// Represents the Git file action request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitFileActionRequest {
    action: GitFileAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_path: Option<String>,
    path: String,
    #[serde(default)]
    status_code: Option<String>,
    workdir: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

/// Represents the Git commit request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitCommitRequest {
    message: String,
    workdir: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

/// Represents the Git repo action request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitRepoActionRequest {
    workdir: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

// Git responses + supporting enums
/// Represents Git status file.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitStatusFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    index_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_path: Option<String>,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    submodule: Option<GitSubmoduleStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    worktree_status: Option<String>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitSubmoduleStatus {
    checkout: GitSubmoduleCheckout,
    // Each flag describes whether that section compares a gitlink on either
    // side. A type change can therefore have just one submodule row.
    staged: bool,
    unstaged: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    staged_commits: Option<GitSubmoduleFiles>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unstaged_commits: Option<GitSubmoduleFiles>,
    // Local changes belong to the unstaged row, including edits staged in
    // the submodule itself that do not change the superproject's index.
    #[serde(skip_serializing_if = "Option::is_none")]
    modified: Option<GitSubmoduleFiles>,
    #[serde(skip_serializing_if = "Option::is_none")]
    untracked: Option<GitSubmoduleFiles>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum GitSubmoduleCheckout {
    #[default]
    CheckedOut,
    NotCheckedOut,
    Unavailable,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
enum GitSubmoduleFiles {
    Listed {
        paths: Vec<String>,
        remaining: usize,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        incomplete: bool,
    },
    MissingCommit {
        commit: String,
    },
    Unavailable,
}

/// Represents the Git status response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitStatusResponse {
    ahead: usize,
    behind: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    files: Vec<GitStatusFile>,
    is_clean: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    upstream: Option<String>,
    workdir: String,
}

/// Defines the Git diff section variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum GitDiffSection {
    Staged,
    Unstaged,
}

impl GitDiffSection {
    /// Returns the key representation.
    fn as_key(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Unstaged => "unstaged",
        }
    }

    fn summary_label(self) -> &'static str {
        match self {
            Self::Staged => "Staged",
            Self::Unstaged => "Unstaged",
        }
    }
}

/// Enumerates Git file actions.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum GitFileAction {
    Stage,
    Unstage,
    Revert,
}

/// Represents the Git diff request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitDiffRequest {
    original_path: Option<String>,
    path: String,
    section_id: GitDiffSection,
    #[serde(default)]
    status_code: Option<String>,
    workdir: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

/// Defines the Git diff change type variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum GitDiffChangeType {
    Edit,
    Create,
}

/// Represents the Git diff response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitDiffResponse {
    change_type: GitDiffChangeType,
    change_set_id: String,
    diff: String,
    diff_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    document_enrichment_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    document_content: Option<GitDiffDocumentContent>,
    summary: String,
}

/// Represents full document sides for a Git diff.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitDiffDocumentContent {
    before: GitDiffDocumentSide,
    after: GitDiffDocumentSide,
    can_edit: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    edit_blocked_reason: Option<String>,
    is_complete_document: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

/// Represents one full document side for a Git diff.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitDiffDocumentSide {
    content: String,
    source: GitDiffDocumentSideSource,
}

/// Defines where a full document side came from.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum GitDiffDocumentSideSource {
    Head,
    Index,
    Worktree,
    Empty,
}

/// Represents the Git commit response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitCommitResponse {
    status: GitStatusResponse,
    summary: String,
}

/// Represents the Git repo action response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitRepoActionResponse {
    status: GitStatusResponse,
    summary: String,
}
