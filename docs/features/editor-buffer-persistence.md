# Feature Spec: Editor Buffer Persistence

## Status

Design — not yet implemented. Prepared alongside the ArrowDown-at-EOF refinement
in `docs/features/markdown-document-view.md` so both share the same keyboard /
cursor vocabulary.

## Problem

Editor state is volatile. Close a tab, reload the browser, or restart the app
and every unsaved edit disappears:

- The user loses their place (scroll position + cursor).
- Their undo history is gone, so edits from an earlier session cannot be rolled
  back.
- For long rendered-Markdown diff review sessions, this means restarting the
  whole review if the window is closed accidentally.

The file system already persists committed changes. What's missing is
*in-flight* editor state — the work that exists only inside the user's
buffer between `last open` and `next save`.

## Goals

- Persist per-tab editor state across browser reload and app restart, without
  any explicit user action.
- Survive React component unmount/remount when the user switches tabs and
  returns. State is not tied to the React tree's lifetime.
- Keep a bounded history of reversible edit commands so Ctrl+Z /
  Ctrl+Shift+Z work across reloads, not just within a single session.
- Apply uniformly to the three editor surfaces that hold editable document
  content: the rendered Markdown diff editor, the Monaco source editor, and
  the Monaco git-diff editor.

## Non-goals (V1)

- Cross-browser or cross-machine sync — localStorage is local to the
  browser profile. Opening the same workspace in a second browser sees fresh
  state there. See [Future Directions](#future-directions) for what a shared
  edit session would look like.
- Collaborative editing. A buffer belongs to one tab.
- Server-side persistence. V1 is browser-only; the backend has no notion of
  per-tab editor state.
- Undoing across a file delete / rename / closed-tab boundary.

## Product Model

### Persistence key: tab id

State is keyed by the **tab id**, not by file path. The tab is the durable
identity in the workspace tree — it already roams across pane drag/drops and
persists inside the workspace layout.

Consequences:

- Closing the tab drops the buffer. The user opted out explicitly.
- Reopening the same file in a new tab starts fresh. Two tabs on the same
  file keep independent state.
- If the underlying workspace is moved or switched, persisted buffers for tabs
  that no longer exist are evicted on next load.

### Covered editors

| Editor | File | Surface |
| :--- | :--- | :--- |
| MarkdownDiffView | `ui/src/panels/DiffPanel.tsx` | Rendered Markdown diff, `contentEditable` |
| MonacoCodeEditor | `ui/src/MonacoCodeEditor.tsx` | Source panel single-file Monaco |
| MonacoDiffEditor | `ui/src/MonacoDiffEditor.tsx` | Git-diff two-side Monaco |

All three hold editable content and accept user input. All three already have
an internal undo ring that lives and dies with the editor instance; this spec
makes that ring durable.

### Persisted state shape

Per tab:

1. **Scroll offset.** Pixel `scrollTop` for Monaco; pixel `scrollTop` on the
   `.markdown-diff-change-scroll` container for MarkdownDiffView.
2. **Cursor position.** Editor-specific:
   - Monaco: `{ lineNumber, column }` (and for diff editor, which side the
     caret was on).
   - MarkdownDiffView: an anchor path — section id + text-node offset or a
     `data-markdown-line-start` + intra-block offset — that can be replayed
     after the DOM is re-rendered from Markdown source.
3. **Current buffer content.** The working copy, regardless of whether it
   has been saved.
4. **Command log.** A ring buffer of reversible edit operations, capped at
   200 entries. Each entry is enough to undo the edit (either a structured
   diff patch against the previous state or Monaco's native undo ring
   entries serialized).

### Save semantics

**Save does not clear the history.** After a successful save the buffer
matches the on-disk file, but the command log is preserved so the user can
still Undo back across the save point. Rationale: users sometimes save
prematurely and then realise they want to revert a change — the undo log
should outlive a single save.

Save does:

- update the stored `contentHash` to match the just-saved file, so conflict
  detection on rehydrate compares against the right baseline;
- push a "save marker" entry into the command log so the undo UI can show a
  breadcrumb ("Undo past save" vs. "Undo in-flight edit").

### Tab-close semantics

Closing a tab evicts its persisted entry from `localStorage`. This is the
explicit opt-out: if the user wanted to keep editing later, they should
keep the tab open.

If localStorage is purged by the browser (user clears site data, incognito
session ends, quota exceeded), the buffer is gone too. This matches the
implicit promise of localStorage-backed state everywhere else in the app.

### Conflict handling

Covered by [File Change Awareness](./file-change-awareness.md). When a
persisted buffer is rehydrated and its `contentHash` no longer matches the
disk baseline:

- If a conservative line-based rebase succeeds, the buffer is applied on top
  of the new disk version and the command log stays valid (commands replay
  against the new baseline).
- If the rebase conflicts, the buffer and command log are preserved
  untouched, and the existing conflict UI (rebase retry / reload from disk /
  save anyway / side-by-side compare) takes over.

The persistence layer does **not** invent new conflict semantics. It just
ensures the existing rebase / conflict flow runs with a rehydrated buffer
instead of a fresh one.

## Storage

### localStorage layout

Key pattern:

```
termal-editor-buffer:{workspaceViewId}:{tabId}
```

- `workspaceViewId` scopes per open workspace (matches the existing workspace
  layout key — `termal-workspace-layout-{workspaceViewId}`).
- `tabId` is the workspace tab identifier.

Value is a JSON blob shaped like:

```ts
type PersistedEditorBuffer = {
  version: 1;
  editor: "monaco-code" | "monaco-diff" | "markdown-diff";
  tabId: string;
  filePath: string;
  contentHash: string;        // for conflict detection on rehydrate
  scrollOffset: number;
  cursor: EditorCursor;       // discriminated union per editor
  content: string;            // current buffer value
  commands: PersistedCommand[]; // ring, capped at 200
  savedAt: number | null;     // epoch ms of the last save, or null if never saved
  updatedAt: number;          // epoch ms of the last write
};
```

### Eviction

- **Tab close.** Primary eviction trigger.
- **Workspace close** (the parent workspace disappears). Sweep all
  `termal-editor-buffer:{workspaceViewId}:*` keys.
- **Quota exceeded.** Fall back to LRU by `updatedAt`. If we cannot save a
  new entry, we drop the oldest entry and retry; the user is notified if
  more than N evictions happen in one session.

### Size cap

Per-tab cap TBD during implementation. Likely ~1 MB — enough for a large
document + 200 command entries, but small enough that one misbehaving tab
cannot exhaust localStorage.

## Write discipline

- **Debounced.** Writes are coalesced through a short debounce (100–200 ms)
  to avoid hammering localStorage on every keystroke.
- **Blurred first.** On tab/editor blur, flush synchronously so switching
  tabs does not risk losing the trailing write.
- **Unload safe.** On `visibilitychange: hidden` and `beforeunload`, flush
  pending writes synchronously. localStorage is synchronous, so the flush
  itself is reliable; the challenge is making sure we have the latest state
  in memory when the event fires.

## Rehydration discipline

- On tab mount, look up `termal-editor-buffer:{workspaceViewId}:{tabId}`.
- If absent, mount fresh (current behaviour).
- If present but `contentHash` matches disk, apply `content`, then restore
  `scrollOffset` and `cursor`, and seed the editor's undo stack from
  `commands`.
- If present and `contentHash` differs, run the rebase pipeline from
  [File Change Awareness](./file-change-awareness.md) with the persisted
  buffer as input.

## Command-log format

**Still to be nailed down.** Two candidates:

1. **Monaco-native serialization.** Monaco already exposes `pushEditOperations`
   and maintains an undo ring. Serialize entries as
   `{ range, text, forceMoveMarkers }`. Trivial for Monaco editors; we have
   to translate to/from the same shape for MarkdownDiffView.
2. **Structured diff patches.** Each entry is `{ rangeBefore, rangeAfter,
   textBefore, textAfter }` computed from a before/after snapshot. Heavier
   to produce but editor-agnostic.

The structured-patch form is more portable and makes the "shared edit
session" future direction easier, at the cost of extra computation on every
edit. V1 likely leans on the Monaco-native form for Monaco editors and a
structured-patch form for MarkdownDiffView, then unifies later.

## Open questions

- **Command log granularity.** Per keystroke is too noisy, per blur is too
  coarse, per paste / per enter / per compound input event is probably the
  right middle ground. Match what Monaco's own undo ring does by default.
- **Save markers in the log.** Are they first-class entries that count
  against the 200 cap, or sidebars that don't? First-class is simpler.
- **Ring overflow.** When a user exceeds 200 commands, do we silently drop
  the oldest (the "forget the first few edits" UX is already the Monaco
  default) or warn them? Silent + oldest is the default answer unless the
  user wants a breadcrumb.
- **Undo across save.** The spec says "yes" — but should the UI visually
  distinguish "undoing past a save" (file becomes dirty again) from
  "undoing within the current dirty session" (file stays dirty)?
- **Tab rename / move.** The tab id is stable across drag-and-drop, so
  moving a tab should not evict the buffer. Confirm this holds for all the
  pane-tree operations we already support.

## Future Directions

### Shared edit sessions

One answer to the "localStorage is enough" framing opens an interesting
generalisation: move the buffer to the **backend**. Then:

- Multiple browsers on the same machine can attach to the same tab and see
  the same in-flight edits.
- A second user over the LAN could follow along on a diff review.
- The command log becomes a crdt-style operation stream that can be
  merged, branched, or replayed.

V1 does not build this. V1 keeps everything in localStorage. But picking a
structured-patch command log (candidate 2 above) keeps the door open.

### Cross-device roaming

Backend persistence also unlocks roaming: close the laptop, open the desktop,
pick up the same in-flight edit. Requires authentication, which TermAl does
not have in Phase 1 — so this is post-Phase-1 territory.

## Related

- [Markdown Document View](./markdown-document-view.md) — rendered Markdown
  diff editor whose ArrowDown-at-EOF behaviour is a natural complement to
  this persistence feature.
- [File Change Awareness](./file-change-awareness.md) — existing rebase /
  conflict / stale-write semantics that the persistence layer must reuse
  on rehydrate.
- [Diff Review Workflow](./diff-review-workflow.md) — consumer of the
  rendered Markdown editor; the primary user-facing surface this feature
  hardens.
- [Source Renderers](./source-renderers.md) — inline rendered regions
  (Mermaid, KaTeX) inside the Monaco source editor; persistence must
  preserve scroll and cursor across these view zones.
