---
name: termal-repo
description: Work in the TermAl repository. Use when changing transcript virtualization, session pane scrolling, App live-state adoption and recovery, rendered Markdown diff editing, source renderers, persistence and revision handling, or repo-specific tests and invariants. Read this skill before editing code in this repo so ownership boundaries, scroll contracts, recovery semantics, and verification commands stay consistent.
---

# TermAl Repo

## Read First

- Use [docs/bugs.md](docs/bugs.md) for current open bugs only. Do not treat
  `Also fixed in the current tree` entries as active work.
- Use `docs/features/` for behavior and design notes. Read only the files that
  match the area you are changing:
  - `docs/features/session-virtualized-transcript.md`
  - `docs/features/source-renderers.md`
  - `docs/features/sqlite-session-storage.md`
  - `docs/features/diff-review-workflow.md` when changing diff UX

## Ownership Boundaries

- `ui/src/SessionPaneView.tsx`
  - Own transcript keyboard paging, prompt-send bottom follow, jump-to-latest,
    and pane-level message-stack direct scroll writes.
- `ui/src/panels/VirtualizedConversationMessageList.tsx`
  - Own mounted-page reconciliation, spacer layout, page measurement, scroll
    classification, and mounted-band policy.
- `ui/src/message-stack-scroll-sync.ts`
  - Own the seam for programmatic transcript scroll writes. If the caller
    already knows the intent, include explicit `scrollKind`.
- `ui/src/app-live-state.ts`
  - Own state adoption, resync/reconnect logic, revision gating, and deferred
    open-session recovery intent.
- `ui/src/app-session-actions.ts`
  - Own create/fork/send/session mutation flows. Post-`await` UI updates must
    respect `isMountedRef`.
- `ui/src/panels/DiffPanel.tsx` and `ui/src/panels/markdown-*`
  - Own rendered-Markdown diff editing, LF-normalized offset math, rebase/save
    flow, and commit-range resolution.
- `ui/src/source-renderers.ts`
  - Own renderable-region detection and renderer ids. `MonacoCodeEditor` hosts
    inline zones; it does not invent renderer identity.

## Hard Invariants

### Transcript Virtualization

- Mounted pages are the live reading surface.
- Active reading is grow-first. Compaction belongs to idle.
- Keep the current reserve policy unless intentionally changing behavior:
  - `3` viewports above
  - `3` viewports below
  - `1` extra page below as hysteresis
- Heavy content inside mounted pages renders immediately.
- `PageUp` / `PageDown` are transcript-owned fixed-delta jumps. Do not hand
  them back to browser-native page scroll for session transcripts.
- When a code path writes transcript `scrollTop` directly, emit the matching
  `MESSAGE_STACK_SCROLL_WRITE_EVENT`. If the path is a seek/page jump, provide
  explicit `scrollKind`.

### Session Create/Fork Recovery

- Stale create/fork responses must not open phantom sessions or panes.
- Recovery resync may preserve pending `openSessionId` / `paneId`, but that
  intent must only be consumed when the adopted session list actually contains
  the target session.
- Tests should pin:
  - no open before recovery
  - no open when the first recovery snapshot still omits the session
  - open only when a later adopted snapshot or SSE state includes it

### Rendered Markdown And Source Renderers

- Rendered-Markdown commit math is LF-normalized.
- Preserve original document EOL style on save.
- Generated render output must not serialize back into source.
- Staged read-only rendered Markdown may allow caret/navigation, but not edits.
- Renderer ids must stay stable when edits do not semantically change the
  renderable block. Do not make ids depend on absolute line numbers if a more
  stable identity is available.

### Persistence And Revision Handling

- Revision handling stays monotonic unless a restart/rollback path explicitly
  allows a downgrade.
- Do not reintroduce full-history persistence or state-adoption work on session
  create/fork paths.
- Restart/recovery logic must not accept late state from an older server
  instance once a newer instance has been adopted.

## Area-Specific Checks

- Always run:
  - `cargo check`
  - `cd ui && npx tsc --noEmit`

- If changing transcript scroll, paging, mounted-range, or prompt-follow:
  - `cd ui && npx vitest run src/panels/AgentSessionPanel.test.tsx`
  - `cd ui && npx vitest run src/App.scroll-behavior.test.tsx`

- If changing create/fork/recovery/adoption behavior:
  - `cd ui && npx vitest run src/App.session-lifecycle.test.tsx`

- If changing rendered Markdown, diff edit, or commit-range logic:
  - `cd ui && npx vitest run src/panels/DiffPanel.test.tsx`
  - `cd ui && npx vitest run src/panels/markdown-diff-segments.test.ts`
  - `cd ui && npx vitest run src/panels/markdown-commit-ranges.test.ts`
  - `cd ui && npx vitest run src/MarkdownContent.test.tsx`

- If changing source renderers or inline zones:
  - `cd ui && npx vitest run src/source-renderers.test.ts`
  - plus the relevant SourcePanel/Monaco tests

## Editing Guidance

- Prefer small helpers over adding more stateful branches to already-large UI
  files, but do not break the ownership boundaries above.
- Update the relevant `docs/features/*.md` file when behavior changes.
- Update `docs/bugs.md` only when an issue is actually fixed, newly introduced,
  or narrowed by real evidence.
- Keep user-visible keyboard behavior explicit. If a shortcut is pane-owned,
  keep editable-target guards and active-pane ownership intact.
- Do not commit or push unless the user explicitly asks.

## Good Defaults

- Treat the focused `AgentSessionPanel` suite as the release gate for transcript
  virtualization work.
- Treat phantom-session prevention as non-negotiable for create/fork/recovery
  changes.
- Treat rendered-Markdown offset math and EOL preservation as one contract, not
  separate concerns.
