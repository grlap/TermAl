// Git HTTP wire types — requests, responses, and file-level status shapes.
//
// Groups all DTOs consumed or returned by `api_git.rs` routes. Split
// out of wire.rs to keep the shared type vocabulary navigable.

use super::*;

// Git requests

/// Represents the Git file action request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitFileActionRequest {
    pub(crate) action: GitFileAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) original_path: Option<String>,
    pub(crate) path: String,
    #[serde(default)]
    pub(crate) status_code: Option<String>,
    pub(crate) workdir: String,
    #[serde(default)]
    pub(crate) project_id: Option<String>,
    #[serde(default)]
    pub(crate) session_id: Option<String>,
}

/// Represents the Git commit request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitCommitRequest {
    pub(crate) message: String,
    pub(crate) workdir: String,
    #[serde(default)]
    pub(crate) project_id: Option<String>,
    #[serde(default)]
    pub(crate) session_id: Option<String>,
}

/// Represents the Git repo action request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitRepoActionRequest {
    pub(crate) workdir: String,
    #[serde(default)]
    pub(crate) project_id: Option<String>,
    #[serde(default)]
    pub(crate) session_id: Option<String>,
}

// Git responses + supporting enums
/// Represents Git status file.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitStatusFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) index_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) original_path: Option<String>,
    pub(crate) path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) submodule: Option<GitSubmoduleStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) worktree_status: Option<String>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitSubmoduleStatus {
    pub(crate) checkout: GitSubmoduleCheckout,
    // Each flag describes whether that section compares a gitlink on either
    // side. A type change can therefore have just one submodule row.
    pub(crate) staged: bool,
    pub(crate) unstaged: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) staged_commits: Option<GitSubmoduleFiles>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unstaged_commits: Option<GitSubmoduleFiles>,
    // Local changes belong to the unstaged row, including edits staged in
    // the submodule itself that do not change the superproject's index.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) modified: Option<GitSubmoduleFiles>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) untracked: Option<GitSubmoduleFiles>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum GitSubmoduleCheckout {
    #[default]
    CheckedOut,
    NotCheckedOut,
    Unavailable,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub(crate) enum GitSubmoduleFiles {
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
pub(crate) struct GitStatusResponse {
    pub(crate) ahead: usize,
    pub(crate) behind: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) branch: Option<String>,
    pub(crate) files: Vec<GitStatusFile>,
    pub(crate) is_clean: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) repo_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) upstream: Option<String>,
    pub(crate) workdir: String,
}

/// Defines the Git diff section variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum GitDiffSection {
    Staged,
    Unstaged,
}

impl GitDiffSection {
    /// Returns the key representation.
    pub(crate) fn as_key(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Unstaged => "unstaged",
        }
    }

    pub(crate) fn summary_label(self) -> &'static str {
        match self {
            Self::Staged => "Staged",
            Self::Unstaged => "Unstaged",
        }
    }
}

/// Enumerates Git file actions.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum GitFileAction {
    Stage,
    Unstage,
    Revert,
}

/// Represents the Git diff request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitDiffRequest {
    pub(crate) original_path: Option<String>,
    pub(crate) path: String,
    pub(crate) section_id: GitDiffSection,
    #[serde(default)]
    pub(crate) status_code: Option<String>,
    pub(crate) workdir: String,
    #[serde(default)]
    pub(crate) project_id: Option<String>,
    #[serde(default)]
    pub(crate) session_id: Option<String>,
}

/// Defines the Git diff change type variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum GitDiffChangeType {
    Edit,
    Create,
}

/// Represents the Git diff response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitDiffResponse {
    pub(crate) change_type: GitDiffChangeType,
    pub(crate) change_set_id: String,
    pub(crate) diff: String,
    pub(crate) diff_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) document_enrichment_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) document_content: Option<GitDiffDocumentContent>,
    pub(crate) summary: String,
}

/// Represents full document sides for a Git diff.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitDiffDocumentContent {
    pub(crate) before: GitDiffDocumentSide,
    pub(crate) after: GitDiffDocumentSide,
    pub(crate) can_edit: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) edit_blocked_reason: Option<String>,
    pub(crate) is_complete_document: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<String>,
}

/// Represents one full document side for a Git diff.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitDiffDocumentSide {
    pub(crate) content: String,
    pub(crate) source: GitDiffDocumentSideSource,
}

/// Defines where a full document side came from.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum GitDiffDocumentSideSource {
    Head,
    Index,
    Worktree,
    Empty,
}

/// Represents the Git commit response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitCommitResponse {
    pub(crate) status: GitStatusResponse,
    pub(crate) summary: String,
}

/// Represents the Git repo action response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitRepoActionResponse {
    pub(crate) status: GitStatusResponse,
    pub(crate) summary: String,
}
