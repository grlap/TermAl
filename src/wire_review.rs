// Review document wire types.
//
// DTOs for the code-review feature: a versioned `ReviewDocument`
// with origin metadata, per-file entries, threaded comments
// anchored to text ranges, and author/status enums. Complemented
// by the `ReviewDocumentSummary` shape used by the sidebar badge.
// Consumed by the routes in `api_review.rs` and persisted by
// `review.rs` under `<workdir>/.termal/reviews/…`.

use super::*;

pub(crate) const REVIEW_DOCUMENT_VERSION: u32 = 1;

/// Represents the review document.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewDocument {
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) revision: u64,
    pub(crate) change_set_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) origin: Option<ReviewOrigin>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) files: Vec<ReviewFileEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) threads: Vec<ReviewThread>,
}

/// Represents review origin.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewOrigin {
    pub(crate) session_id: String,
    pub(crate) message_id: String,
    pub(crate) agent: String,
    pub(crate) workdir: String,
    pub(crate) created_at: String,
}

/// Represents review file entry.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewFileEntry {
    pub(crate) file_path: String,
    pub(crate) change_type: ChangeType,
}

/// Represents review thread.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewThread {
    pub(crate) id: String,
    pub(crate) anchor: ReviewAnchor,
    pub(crate) status: ReviewThreadStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) comments: Vec<ReviewThreadComment>,
}

/// Represents review thread comment.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewThreadComment {
    pub(crate) id: String,
    pub(crate) author: ReviewCommentAuthor,
    pub(crate) body: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
}

/// Defines the review anchor variants.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum ReviewAnchor {
    ChangeSet,
    File {
        file_path: String,
    },
    Hunk {
        file_path: String,
        hunk_header: String,
    },
    Line {
        file_path: String,
        hunk_header: String,
        old_line: Option<usize>,
        new_line: Option<usize>,
    },
}

/// Defines the review comment author variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ReviewCommentAuthor {
    User,
    Agent,
}

/// Enumerates review thread states.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ReviewThreadStatus {
    Open,
    Resolved,
    Applied,
    Dismissed,
}

/// Represents the review document response payload.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewDocumentResponse {
    pub(crate) review_file_path: String,
    pub(crate) review: ReviewDocument,
}

/// Represents the review summary response payload.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewSummaryResponse {
    pub(crate) change_set_id: String,
    pub(crate) review_file_path: String,
    pub(crate) thread_count: usize,
    pub(crate) open_thread_count: usize,
    pub(crate) resolved_thread_count: usize,
    pub(crate) comment_count: usize,
    pub(crate) has_threads: bool,
}

/// Summarizes review document.
#[derive(Default)]
pub(crate) struct ReviewDocumentSummary {
    pub(crate) thread_count: usize,
    pub(crate) open_thread_count: usize,
    pub(crate) resolved_thread_count: usize,
    pub(crate) comment_count: usize,
}
