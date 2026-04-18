# Feature Brief: Diff Review Workflow

Backlog source: [`docs/bugs.md`](../bugs.md)

The detailed diff-preview and review-plan content below was moved out of `docs/bugs.md`.

Related:
- [Markdown Document View](./markdown-document-view.md) — rendered
  Markdown diff editor underneath this workflow, with inline
  Mermaid / KaTeX rendering for review sessions.
- [Editor Buffer Persistence](./editor-buffer-persistence.md) — durable
  in-flight state (scroll, cursor, undo history) for review-mode edits
  that survive reloads and tab switches.
- [File Change Awareness](./file-change-awareness.md) — stale-save,
  rebase, and conflict semantics the review editor reuses unchanged.

## Agent replies in diff review comments

**Severity:** Medium â€” closes the review feedback loop.

When the user leaves review comments on a diff preview and hands them off to an agent, the agent
currently has no way to reply inline. Comments are one-directional: user writes, agent reads.

**Desired behavior:**
- When an agent addresses a review comment, it can post a reply on the same anchor
- Agent replies appear inline in the diff preview alongside the user's original comment
- Each comment thread shows the back-and-forth (user comment â†’ agent reply â†’ user follow-up)
- Agent replies set the comment status to `resolved` or leave it `open` with an explanation

**Tasks:**
- Extend the review comment schema to support threaded replies with an `author` field (`user` or `agent`)
- Add a backend endpoint or convention for the agent to append replies to an existing review file
- Update the diff preview UI to render comment threads instead of single comments
- Update the agent handoff prompt to instruct the agent to write replies, not just resolve silently



This is the concrete delivery plan for the diff-preview and saved-review workflow.

## Goals

- Let the user open a structured diff preview in a new tab directly from an agent update.
- Let the preview link back to the exact conversation session and message that produced the change.
- Let the user add PR-style review comments and save them to disk.
- Let a later agent turn find the saved review file and act on unresolved comments.

## Non-goals for v1

- No browser URL routing or shareable deep links yet.
- No multi-user review system.
- No git-hosted PR sync.
- No dependency on an external diff viewer library in the first pass.

## Current constraints

- The frontend has no router today; navigation is entirely workspace-state driven.
- Workspace tabs are effectively session tabs today, with source view as a pane mode rather than a
  first-class tab entity.
- `DiffMessage` only carries `filePath`, `summary`, `diff`, and `changeType`.
- Backend state updates are already pushed through `/api/events` SSE snapshots, which is sufficient
  for this feature.
- The UI has no diff rendering dependency today, so phase 1 should use a small internal unified
  diff parser and renderer.

## Proposed architecture

### 1. Link target system

Introduce a typed in-app link system so navigation is explicit and reusable.

```ts
type LinkTarget =
  | { kind: "session"; sessionId: string }
  | { kind: "message"; sessionId: string; messageId: string }
  | { kind: "source"; path: string }
  | { kind: "diffPreview"; changeSetId: string; originSessionId: string; originMessageId: string };
```

Rules:
- All in-app navigation goes through one `openLink(target, options)` helper.
- The helper decides whether to focus an existing tab or open a new one.
- The first version only needs in-memory app navigation, not browser history.

### 2. Generic workspace tabs

Refactor the workspace model so a pane can hold first-class tabs instead of only session IDs.

```ts
type WorkspaceTab =
  | { id: string; kind: "session"; sessionId: string }
  | { id: string; kind: "source"; path: string }
  | {
      id: string;
      kind: "diffPreview";
      changeSetId: string;
      originSessionId: string;
      originMessageId: string;
    };
```

Rules:
- Deduping for diff preview tabs should key off `changeSetId`.
- A pane can still default to opening session tabs the same way it does today.
- Source view should migrate into the same tab model so navigation remains consistent.

### 3. Change-set identity

Every diff preview needs a stable ID that survives reopening and review-file lookup.

Proposal:
- Add `changeSetId` to `DiffMessage`.
- For v1, generate one change set per diff message.
- When the backend later groups multiple file diffs from one response, multiple `DiffMessage`
  entries can share the same `changeSetId`.

Minimum backend metadata to add to diff-like messages:
- `originSessionId`
- `originMessageId`
- `changeSetId`
- optional `turnId` if a turn-level grouping ID becomes useful later

## Diff preview plan

### Rendering model

Phase 1:
- Parse unified diff text in the frontend into `files -> hunks -> lines`.
- Render a structured viewer with:
  file header
  change type
  hunk header
  old/new line numbers
  added/removed/context styling
- Keep a "Raw patch" toggle for debugging and fallback.

Phase 2:
- Support grouped previews for all file diffs in the same response.
- Add better context collapsing and optional split view if needed.

### Why not add a library first

The repo currently has no diff-viewer dependency and no routing layer. The first implementation
should minimize moving pieces:
- build a small parser for the subset of unified diffs TermAl already emits
- validate the UX
- only introduce a third-party diff library if the internal renderer becomes a maintenance burden

## Review comment plan

### Comment scopes

Support these scopes in v1:
- change-set level comment
- file-level comment
- hunk-level comment
- line-level comment

### Stable anchors

Never anchor comments to DOM position. Use structured targets:

```ts
type ReviewAnchor =
  | { kind: "changeSet" }
  | { kind: "file"; filePath: string }
  | { kind: "hunk"; filePath: string; hunkHeader: string }
  | {
      kind: "line";
      filePath: string;
      hunkHeader: string;
      oldLine: number | null;
      newLine: number | null;
    };
```

### Review file format

Persist review state under the existing TermAl workspace data directory:

```text
.termal/
  sessions.json
  reviews/
    <change-set-id>.json
    <change-set-id>.md   # optional export later
```

Proposed JSON schema:

```json
{
  "version": 1,
  "changeSetId": "change-session-3-message-42",
  "origin": {
    "sessionId": "session-3",
    "messageId": "message-42",
    "agent": "Codex",
    "workdir": "/Users/greg/GitHub/Personal/termal",
    "createdAt": "2026-03-09T18:55:00Z"
  },
  "files": [
    {
      "filePath": "docs/bugs.md",
      "changeType": "edit"
    }
  ],
  "comments": [
    {
      "id": "comment-1",
      "anchor": {
        "kind": "line",
        "filePath": "docs/bugs.md",
        "hunkHeader": "@@ -10,3 +10,8 @@",
        "oldLine": null,
        "newLine": 17
      },
      "body": "This should mention Codex attachment support explicitly.",
      "status": "open",
      "author": "user",
      "createdAt": "2026-03-09T19:02:00Z",
      "updatedAt": "2026-03-09T19:02:00Z"
    }
  ]
}
```

Rules:
- `status` starts as `open`; allowed values: `open`, `resolved`, `applied`, `dismissed`.
- The agent should treat only `open` comments as active review feedback by default.
- The file should be fully replaceable on save to keep backend logic simple in v1.

## API plan

Add a small review API beside the existing session routes.

Suggested routes:
- `GET /api/reviews/{changeSetId}`
- `PUT /api/reviews/{changeSetId}`
- `GET /api/reviews/{changeSetId}/summary`

Notes:
- `PUT` can save the whole review document for v1 instead of building comment-level mutation
  endpoints.
- Review save/load does not need a separate realtime channel; normal UI state can refresh after
  save.
- If the frontend only needs the saved file path for handoff, the backend can also return
  `reviewFilePath` in the review payload.

## UI plan

### Conversation surface

- Add `Open preview` to each diff card.
- If multiple diff cards later share one `changeSetId`, the action can open the grouped preview.
- Add a subtle saved-review indicator when comments already exist for that change set.

### Diff preview tab

Header actions:
- `Back to conversation`
- `Copy review file path`
- `Insert review into prompt`
- `Raw patch`

Body:
- structured diff viewer
- inline comment affordances
- review sidebar or footer listing open and resolved comments

### Backlink behavior

- `Back to conversation` focuses the origin session tab if it is already open.
- If the origin session tab is not open, open it in the current pane and scroll to the origin
  message.
- Highlight the origin message briefly so the jump is visually obvious.

## Agent handoff plan

The point of saving review files is to make them usable in later turns without manual copy-paste.

V1 handoff flow:
1. User opens diff preview.
2. User adds comments and saves review.
3. UI shows the saved review path, for example
   `.termal/reviews/change-session-3-message-42.json`.
4. User clicks `Insert review into prompt`.
5. The composer gets a short, structured handoff message like:
   `Please address the open review comments in .termal/reviews/change-session-3-message-42.json`
6. The later agent turn reads the file from disk and resolves comments one by one.

## Implementation phases

### Phase 1: metadata and navigation

- Add `originSessionId`, `originMessageId`, and `changeSetId` to diff messages.
- Refactor frontend workspace state to generic tabs.
- Add `LinkTarget` and `openLink()`.
- Add `Open preview` and `Back to conversation`.

### Phase 2: diff preview

- Implement a small unified diff parser in the frontend.
- Render a structured diff preview tab.
- Keep raw patch fallback.
- Add tab dedupe by `changeSetId`.

### Phase 3: saved review comments

- Add backend review store under `.termal/reviews/`.
- Add `GET`/`PUT` review routes.
- Add inline and file-level comment UI.
- Save and reload review documents.

### Phase 4: agent handoff and polish

- Add `Insert review into prompt`.
- Add saved-review indicators in conversation cards.
- Add resolved/open filtering.
- Add optional Markdown export only if JSON proves too opaque for manual inspection.

## Testing plan

Backend:
- review file save/load round-trip
- invalid review payload rejection
- missing review file returns empty/default state
- diff message serialization includes origin metadata

Frontend:
- `Open preview` opens the correct diff preview tab
- re-opening the same change set focuses instead of duplicating
- backlink opens the right session and highlights the right message
- diff parser handles create and edit patches
- comment anchors survive reload from saved JSON

Integration:
- Claude-generated diff can open preview, save comments, and insert review path into prompt
- Codex-generated diff can do the same

## Acceptance criteria

- Clicking `Open preview` on a diff-related agent update opens a new diff preview tab.
- The preview tab can navigate back to the originating conversation message.
- The diff view is structured and readable without forcing the user to parse raw patch text.
- Review comments can be added at change-set, file, hunk, and line scope.
- Review comments persist to `.termal/reviews/<changeSetId>.json`.
- A later agent turn can be pointed at that file and identify open comments without ambiguity.

