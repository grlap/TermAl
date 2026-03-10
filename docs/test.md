# Test Plan

Testing strategy for TermAl, prioritized by risk and value. The codebase currently has zero tests
and zero test infrastructure in both the frontend and backend.

## Constraints

- `ui/src/App.tsx` is a 3,300-line monolithic component — component extraction is a prerequisite
  for component-level UI tests, but can happen incrementally alongside feature work.
- `src/main.rs` is a 6,400-line monolithic file — Rust unit tests can live in the same file via
  `#[cfg(test)] mod tests { ... }` without needing a refactor.
- No existing test scripts, test configs, or test dependencies.

---

## Phase 1: Test Infrastructure Setup

### Frontend — Vitest

Vitest runs on the existing Vite config with near-zero setup.

**Install:**
```bash
cd ui
npm install -D vitest jsdom @testing-library/react @testing-library/user-event
```

**Add to `ui/package.json` scripts:**
```json
{
  "scripts": {
    "test": "vitest run",
    "test:watch": "vitest"
  }
}
```

**Add to `ui/vite.config.ts`:**
```ts
/// <reference types="vitest" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: "./src/test-setup.ts",
  },
  // ... existing server config
});
```

**Create `ui/src/test-setup.ts`:**
```ts
import "@testing-library/jest-dom";
```

This installs the matcher extensions (`toBeInTheDocument`, `toHaveTextContent`, etc.).

Optionally install the types:
```bash
npm install -D @testing-library/jest-dom
```

**Estimated effort:** 1-2 hours.

### Backend — Cargo Test

Rust's built-in test framework requires no setup. Tests go in `src/main.rs` at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example() {
        assert_eq!(1 + 1, 2);
    }
}
```

Run with:
```bash
cargo test
```

For async handler tests, add `axum-test` or use `tower::ServiceExt`:
```toml
[dev-dependencies]
axum-test = "16"
tokio-test = "0.4"
```

**Estimated effort:** 30 minutes.

---

## Phase 2: Pure Logic Tests (no DOM, no React)

These are plain `.test.ts` files that import TypeScript functions and assert on return values.
They are fast, stable, and give the highest confidence-per-line-of-test-code.

**These require extracting pure functions into importable modules first.** Each extraction is
small and self-contained — pull a function out of `App.tsx` into its own file, import it back,
and write tests for the extracted version.

### 2a. Workspace State Logic

The workspace model (pane splitting, tab management, deduplication) is the riskiest area to
refactor for the diff viewer feature. Test it first.

**Extract to:** `ui/src/workspace.ts`

**Functions to extract and test:**

| Function | What it does | Why it matters |
|----------|-------------|----------------|
| `createPane()` | Initialize a new pane with defaults | Foundation for generic tabs |
| `splitPane()` | Split a pane into two with a direction and ratio | Core layout operation |
| `addSessionToPane()` | Add a session tab to a pane | Must survive tab model refactor |
| `removeSessionFromPane()` | Remove a session tab, select next active | Edge cases: last tab, active tab removed |
| `moveSessionBetweenPanes()` | Drag-and-drop tab transfer | Must not lose tabs or create duplicates |
| `findPaneForSession()` | Given a session ID, find which pane holds it | Used everywhere for focus and navigation |
| `deduplicateTab()` | Prevent duplicate tabs for the same entity | Critical for diff preview tabs keyed by `changeSetId` |

**Test cases:**

```
workspace.test.ts

- createPane returns a pane with empty sessionIds and null activeSessionId
- splitPane produces a split node with two child panes
- splitPane preserves the original pane's sessions in the first child
- addSessionToPane appends to sessionIds and sets activeSessionId
- addSessionToPane does not duplicate an already-present session
- removeSessionFromPane removes the session and selects the next tab
- removeSessionFromPane on the last session sets activeSessionId to null
- moveSessionBetweenPanes removes from source and adds to target
- moveSessionBetweenPanes does not duplicate if target already has the session
- findPaneForSession returns the correct pane
- findPaneForSession returns null for an unknown session
```

**Estimated effort:** 1 day (extract + write tests).

### 2b. Diff Parser

This does not exist yet. Build it as a standalone module with tests from day one.

**Create:** `ui/src/diff-parser.ts`

**Types:**

```ts
type DiffFile = {
  oldPath: string;
  newPath: string;
  hunks: DiffHunk[];
};

type DiffHunk = {
  header: string;       // e.g. "@@ -10,3 +10,8 @@"
  oldStart: number;
  oldCount: number;
  newStart: number;
  newCount: number;
  lines: DiffLine[];
};

type DiffLine = {
  type: "add" | "remove" | "context";
  content: string;
  oldLineNumber: number | null;
  newLineNumber: number | null;
};
```

**Test cases:**

```
diff-parser.test.ts

Parsing basics:
- parses a single-file edit with one hunk
- parses a single-file edit with multiple hunks
- parses a new file (--- /dev/null)
- parses a deleted file (+++ /dev/null)
- parses multiple files in one diff
- preserves leading whitespace in content lines
- handles empty context lines
- handles no-newline-at-end-of-file marker

Line numbering:
- context lines have both old and new line numbers
- added lines have only new line numbers
- removed lines have only old line numbers
- line numbers are continuous within a hunk
- line numbers reset at each hunk based on header

Hunk header parsing:
- extracts oldStart, oldCount, newStart, newCount from standard header
- handles header with optional section name (@@ -1,3 +1,5 @@ function foo)
- handles count of 0 for empty side
- handles missing count (implied 1)

Edge cases:
- empty diff string returns empty array
- diff with only file headers and no hunks returns file with empty hunks
- binary file marker is handled gracefully
- rename detection (rename from / rename to)
```

**Estimated effort:** 2 days (build parser + tests together).

### 2c. Review Anchor Resolution

Given a comment anchor and a parsed diff, resolve the anchor to a specific location.

**Create:** `ui/src/review-anchors.ts`

**Test cases:**

```
review-anchors.test.ts

- changeSet anchor matches any position in the diff
- file anchor matches the correct DiffFile by filePath
- file anchor returns null for unknown filePath
- hunk anchor matches by filePath + hunkHeader
- hunk anchor returns null for unknown hunkHeader
- line anchor matches by filePath + hunkHeader + newLine
- line anchor matches by filePath + hunkHeader + oldLine
- line anchor with both oldLine and newLine matches the exact line
- line anchor returns null when line is outside hunk range
- anchors survive round-trip through JSON serialization
```

**Estimated effort:** 0.5 day.

### 2d. Link Target Navigation

Test that `openLink()` produces the correct workspace state transitions.

**Create:** `ui/src/navigation.ts`

**Test cases:**

```
navigation.test.ts

- openLink session target focuses existing pane if session is already open
- openLink session target opens session in active pane if not already open
- openLink message target focuses session pane and sets scroll target
- openLink diffPreview target opens a new diff preview tab
- openLink diffPreview target focuses existing tab if changeSetId matches
- openLink diffPreview does not duplicate tabs
- openLink source target opens source view in active pane
```

**Estimated effort:** 0.5 day.

---

## Phase 3: React Component Tests

These require `jsdom`, `@testing-library/react`, and extracted components. Each component must
be in its own file to be importable in tests.

**Extraction order** (smallest to largest, to keep risk low):

### 3a. Message Card Components

Pull each card renderer out of `App.tsx` into `ui/src/components/`:

| Component | Extract from | Lines (approx) |
|-----------|-------------|-----------------|
| `DiffCard` | `App.tsx` | ~20 lines today, ~200 after diff viewer |
| `ThinkingCard` | `App.tsx` | ~30 lines |
| `CommandCard` | `App.tsx` | ~40 lines |
| `MarkdownCard` | `App.tsx` | ~25 lines |
| `ApprovalCard` | `App.tsx` | ~50 lines |
| `MessageMeta` | `App.tsx` | ~15 lines |

**DiffCard test cases (post diff-viewer build):**

```
DiffCard.test.tsx

Rendering:
- renders file path in header
- renders summary text
- renders "New file" label for create changeType
- renders "File edit" label for edit changeType
- renders structured diff lines with correct add/remove/context classes
- renders line numbers for each diff line
- renders hunk headers between hunks
- collapse button hides diff content
- expand button shows diff content

Interactions:
- clicking a line number shows the comment input
- clicking "Open preview" calls onOpenPreview with correct changeSetId
- comment input submits on Enter (or Cmd+Enter)
- comment input cancels on Escape
```

### 3b. Diff Preview Tab

```
DiffPreviewTab.test.tsx

Rendering:
- renders all files from the change set
- renders "Back to conversation" button
- renders "Copy review file path" button
- renders "Raw patch" toggle
- raw patch toggle shows unformatted diff text
- renders existing comments at their anchor positions
- renders comment count badge per file

Navigation:
- "Back to conversation" calls openLink with correct session and message target
- clicking a file header scrolls to that file section

Comments:
- clicking a line gutter opens inline comment form
- submitting a comment calls the review API
- new comment appears at the correct anchor after save
- resolving a comment updates its status badge
- comment thread shows replies in order
```

### 3c. Review Comment Components

```
InlineCommentForm.test.tsx

- renders a textarea and submit button
- submit button is disabled when textarea is empty
- submitting calls onSubmit with the comment body
- Escape key calls onCancel
- Cmd+Enter submits the form

CommentThread.test.tsx

- renders comment body and author
- renders timestamp
- shows "open" status indicator for open comments
- shows "resolved" status indicator for resolved comments
- clicking resolve calls onResolve with comment id
- renders multiple replies in chronological order
```

**Estimated effort for all component tests:** 2-3 days.

---

## Phase 4: Backend Tests (Rust)

All tests go in `src/main.rs` inside `#[cfg(test)] mod tests { ... }` unless the file is
split into modules first.

### 4a. Data Model Serialization

```rust
#[test] fn diff_message_serializes_with_change_set_id()
#[test] fn diff_message_serializes_with_origin_session_id()
#[test] fn diff_message_serializes_with_origin_message_id()
#[test] fn diff_message_round_trips_through_json()
#[test] fn review_file_round_trips_through_json()
#[test] fn review_comment_statuses_serialize_correctly()
#[test] fn review_anchor_variants_all_serialize()
#[test] fn invalid_review_json_returns_error()
```

### 4b. Review Persistence

```rust
#[test] fn save_review_creates_file_in_reviews_dir()
#[test] fn load_review_returns_saved_content()
#[test] fn load_missing_review_returns_not_found()
#[test] fn save_review_overwrites_existing_file()
#[test] fn review_file_name_matches_change_set_id()
#[test] fn review_dir_is_created_if_missing()
```

### 4c. API Route Tests

Requires `axum-test` or `tower::ServiceExt` for sending requests to the router without
starting a server.

```rust
#[tokio::test] async fn get_review_returns_404_for_unknown_change_set()
#[tokio::test] async fn put_review_creates_new_review()
#[tokio::test] async fn put_review_updates_existing_review()
#[tokio::test] async fn get_review_returns_saved_review()
#[tokio::test] async fn get_review_summary_returns_comment_counts()
#[tokio::test] async fn put_review_rejects_invalid_json()
```

### 4d. Existing Functionality (regression safety)

```rust
#[tokio::test] async fn create_session_returns_new_session()
#[tokio::test] async fn send_message_to_unknown_session_returns_404()
#[tokio::test] async fn get_state_returns_all_sessions()
#[tokio::test] async fn health_endpoint_returns_ok()
#[tokio::test] async fn codex_rate_limit_notification_is_handled()
```

**Estimated effort for all backend tests:** 2 days.

---

## Phase 5: Integration Tests

These verify end-to-end flows across frontend and backend. Run against a real (or mock)
backend instance.

### Option A: Mock API in Vitest

Use `msw` (Mock Service Worker) to intercept fetch calls:

```bash
cd ui && npm install -D msw
```

```
integration/review-flow.test.tsx

- open diff preview → add comment → save → reload → comment is still there
- open diff preview → add comment → resolve → status updates
- "Insert review into prompt" inserts the correct path into composer state
- opening the same changeSetId twice focuses the existing tab
```

### Option B: Full-stack with a test harness

Spawn the Rust backend in test mode, run Vitest against it. More realistic but slower and
more complex to set up. Defer to later.

**Estimated effort:** 1-2 days (Option A only).

---

## Effort Summary

| Phase | What | Effort | Prerequisite |
|-------|------|--------|-------------|
| **1** | Install Vitest + Cargo test setup | 2 hours | None |
| **2a** | Workspace state logic tests | 1 day | Extract workspace functions to `workspace.ts` |
| **2b** | Diff parser (build + test) | 2 days | None (greenfield) |
| **2c** | Review anchor resolution tests | 0.5 day | Depends on diff parser types |
| **2d** | Link target navigation tests | 0.5 day | Extract navigation to `navigation.ts` |
| **3** | React component tests | 2-3 days | Extract components from `App.tsx` |
| **4** | Rust backend tests | 2 days | None (inline `#[cfg(test)]`) |
| **5** | Integration tests | 1-2 days | Phases 1-4 |
| | **Total** | **~9-11 days** | |

### Recommended order for the diff viewer project

1. Phase 1 (infra) — do first, takes 2 hours
2. Phase 2a (workspace tests) — do before the tab model refactor
3. Phase 2b (diff parser) — do as part of building the parser
4. Phase 4a-4b (Rust serialization + persistence) — do as part of building the review backend
5. Phase 3 (component tests) — do after extracting components during the diff viewer build
6. Phase 2c, 2d, 4c, 5 — do last, these are lower risk

### What not to test

- CSS styling and visual appearance — use manual review
- SSE event streaming — integration-level concern, defer
- Claude/Codex runtime spawning — requires real binaries, mock at the boundary
- Drag-and-drop tab reordering — pointer event sequences are brittle in jsdom
