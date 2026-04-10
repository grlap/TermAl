# Feature Brief: File Change Awareness

Backlog source: proposed feature brief; not yet linked from `docs/bugs.md`.

## Problem

Git diff and file editing both work, but the user does not get immediate
visibility when an agent modifies files on disk. If a file is open in the
TermAl editor, the visible buffer can become stale without any obvious signal.

The risk is worse when the user is also editing the same file:

- the agent may change the file on disk while the user's editor buffer is dirty
- the user may keep editing stale content without realizing the file changed
- a save from the UI can overwrite the agent's changes unless the write path
  checks that the editor's base version is still current

TermAl needs a file-change awareness layer between the agent, disk, editor, and
git state.

## Goals

- Surface agent file edits as soon as they happen.
- Protect dirty editor buffers from being overwritten by external disk changes.
- Make stale clean buffers safe to refresh automatically.
- Detect stale writes from the UI and return a clear conflict instead of
  overwriting agent changes.
- Give each agent turn a visible summary of files it changed.

## Non-goals for v1

- No full IDE replacement.
- No collaborative multi-user editing.
- No full visual three-way merge editor in the first version.
- No filesystem watching outside active workspace roots.
- No git-hosted PR or branch workflow dependency.

## Core idea

Introduce a file-change awareness pipeline:

```text
filesystem watcher / git status
  -> backend coalesced change events
  -> SSE file-change notifications
  -> editor buffer freshness checks
  -> user-visible badges, banners, and diff entry points
```

The guiding rule is:

**Never auto-overwrite a dirty user buffer.**

Auto-reload is only safe when the open editor tab has no unsaved local edits.

## User experience

### Agent changes a file that is not open

- The file explorer marks the file as externally changed.
- The current agent turn shows the file in an "Agent changed files" card.
- Clicking the file opens the latest disk version or a diff view.

### Agent changes a clean open file

- The editor reloads the file from disk automatically.
- The tab briefly shows a "Reloaded from disk" status.
- The agent turn's changed-files card still lists the file.

### Agent changes a dirty open file

- TermAl first tries to preserve the user's unsaved edits by rebasing them onto
  the new disk version.
- If the rebase applies cleanly:
  - reload the file from disk
  - reapply the user's unsaved edits on top
  - keep the buffer dirty
  - show "Reloaded external changes and preserved your edits."
- If the rebase conflicts:
  - keep the user's current buffer intact
  - mark the tab as conflicted
  - show "File changed on disk while you were editing."
  - Actions: `Compare`, `Reload disk version`, `Keep mine`, `Save anyway`.

### User saves a stale buffer

- The frontend includes the editor's `baseHash` in the save request.
- The backend compares `baseHash` with the current disk content hash.
- If the disk changed, the backend returns `409 Conflict`.
- The UI opens the same conflict banner and does not overwrite the file unless
  the user explicitly chooses `Save anyway`.

### User edits survive an external reload

The ideal dirty-buffer flow is:

```text
base version user opened
  -> user edits in TermAl
  -> user buffer

base version user opened
  -> agent edits on disk
  -> new disk version

diff(base, user buffer)
  -> apply onto new disk version
  -> rebased user buffer
```

If this applies cleanly, the user sees the agent's disk changes and keeps their
own unsaved edits. The editor stays dirty because the rebased buffer still has
local changes relative to the new disk version.

## Backend plan

### File watching

Add a backend watcher for active workspace roots:

- use the Rust `notify` crate where available
- keep a polling fallback if native watching is unreliable
- watch only active workspace/session roots
- ignore noisy directories such as `.git`, `node_modules`, `target`,
  `.next`, `.vite`, and common build caches
- coalesce events for roughly 250-500ms before broadcasting

Proposed SSE event:

```json
{
  "type": "workspaceFilesChanged",
  "revision": 42,
  "workspaceRoot": "C:\\github\\Personal\\TermAl",
  "changes": [
    {
      "path": "src/runtime.rs",
      "kind": "modified",
      "mtimeMs": 1775860000000,
      "size": 123456,
      "contentHash": "sha256:..."
    }
  ]
}
```

The event should include enough metadata for the frontend to decide whether an
open buffer is stale without immediately fetching every changed file.

### Save preconditions

Extend file write endpoints to accept a base content identity:

```json
{
  "content": "...",
  "baseHash": "sha256:...",
  "overwrite": false
}
```

Rules:

- If `overwrite` is false and the current disk hash differs from `baseHash`,
  return `409 Conflict`.
- If `overwrite` is true, write the submitted content and return the new hash.
- If the file does not exist and the client expected it to exist, return a
  conflict instead of silently recreating unless the user confirms.

Response shape for conflict:

```json
{
  "error": "file changed on disk",
  "kind": "file-conflict",
  "path": "src/runtime.rs",
  "currentHash": "sha256:...",
  "baseHash": "sha256:..."
}
```

### Agent turn changed-file summary

At agent turn start:

- snapshot git status and optionally a lightweight file hash set for watched
  roots

At agent turn completion:

- compute changed files since the turn began
- attach a turn-scoped summary to the session event stream

Initial implementation can use `git diff --name-status` for git worktrees and
fallback to watcher-collected changes for non-git roots.

## Frontend plan

### Editor buffer metadata

Every open file tab should track:

```ts
type FileBufferState = {
  path: string;
  loadedContent: string | null;
  loadedContentHash: string | null;
  loadedMtimeMs: number | null;
  currentBufferHash: string;
  dirty: boolean;
  staleOnDisk: boolean;
  conflict: boolean;
};
```

When a `workspaceFilesChanged` event arrives:

- if the file is not open, update explorer badges only
- if the file is open and `dirty === false`, reload the file from disk
- if the file is open and `dirty === true`, attempt to rebase unsaved edits onto
  the new disk content
- if the rebase applies cleanly, replace the editor content with the rebased
  buffer and keep `dirty: true`
- if the rebase conflicts, mark it as conflicted and keep the user's buffer
  untouched

### Rebase and merge options

There are several implementation choices for preserving user edits after an
external reload.

#### Option 1: Safety-only conflict banner

Behavior:

- dirty buffers are never changed automatically
- external disk changes show a conflict banner
- user manually chooses compare, reload, keep mine, or save anyway

Pros:

- lowest implementation risk
- protects data immediately
- does not require a merge algorithm

Cons:

- poor experience when edits do not overlap
- user has to do manual work even for simple non-conflicting changes

#### Option 2: Text patch reapply

Behavior:

- store the exact `loadedContent` in the editor buffer state
- compute a patch from `loadedContent` to the current user buffer
- apply that patch onto the new disk content after an external change
- if the patch applies cleanly, update the editor to the rebased buffer
- if it fails, fall back to the conflict banner

Pros:

- best MVP balance
- preserves user edits automatically for common non-overlapping changes
- keeps the UI simple
- can run entirely in the frontend where the dirty buffer already lives

Cons:

- patch application can be fragile around large rewrites
- conflict reporting may be coarse at first

#### Option 3: Line-based three-way merge

Behavior:

- store `base`, `mine`, and `theirs`
- run a line-based three-way merge when disk changes
- produce either a clean merged buffer or conflict hunks

Pros:

- more predictable for source code than raw text patching
- better conflict classification
- maps naturally to diff/merge UI later

Cons:

- more complex implementation
- needs careful testing for newline, encoding, and large-file edge cases

#### Option 4: Monaco merge editor

Behavior:

- when a dirty buffer conflicts with disk, open a visual merge view with base,
  user buffer, and disk version
- user resolves conflicts interactively

Pros:

- best final UX for hard conflicts
- familiar IDE workflow

Cons:

- larger integration cost
- not needed for the first safety-focused version

#### Option 5: Backend merge helper

Behavior:

- frontend sends `base`, `mine`, and path/current disk identity to the backend
- backend runs merge logic and returns merged content or conflicts

Pros:

- centralizes merge behavior
- can share logic with non-browser clients later

Cons:

- sends unsaved buffer content through another API roundtrip
- still needs frontend conflict UI
- less natural because the dirty buffer is already in the browser

### Recommended starting point

Start with **Option 2: Text patch reapply**, backed by the safety behavior from
Option 1.

The first implementation should:

- store `loadedContent` for every open editor tab
- on external disk change for a dirty tab, fetch the new disk content
- compute the user's edit patch from `loadedContent` to `currentBuffer`
- try to apply that patch onto the new disk content
- if clean, replace the editor content with the rebased result, update
  `loadedContent` / `loadedContentHash` to the new disk version, and keep the
  tab dirty
- if not clean, keep the user's buffer untouched and show the conflict banner

This gives the "awesome" behavior for the common case without blocking the
first version on a full merge editor. After that works, upgrade the merge
engine toward Option 3 and add Monaco merge UI from Option 4 for conflicts.

### Badges and banners

Add visible state in the file explorer and editor tabs:

- `Modified in git`
- `Changed on disk`
- `Unsaved`
- `Conflict`
- `Changed by this agent turn`

Conflict banner actions:

- `Compare`: open a side-by-side diff between user buffer and disk
- `Reload disk version`: discard local buffer and load disk
- `Keep mine`: keep editing but retain conflict status
- `Save anyway`: send `overwrite: true`

### Agent changed-files card

After an agent turn, add a compact session card:

```text
Agent changed files
M src/runtime.rs
M ui/src/App.tsx
A docs/features/file-change-awareness.md
```

Each row should support:

- open file
- open diff
- copy path

This makes agent edits visible without requiring the user to run `git diff`.

## MVP

Ship the safety path first:

- backend file watcher for active workspace roots
- coalesced `workspaceFilesChanged` SSE event
- editor buffer freshness tracking
- auto-reload clean open tabs
- text-patch reapply for dirty open tabs changed on disk
- conflict banner when dirty-buffer rebase cannot apply cleanly
- `baseHash` / `409 Conflict` protection on file saves
- simple "Agent changed files" card after turn completion

## Later enhancements

- Monaco side-by-side diff for user buffer vs disk.
- Line-based three-way merge using base, user buffer, and disk/agent version.
- Monaco merge editor for conflicted dirty buffers.
- "Review all agent changes" workspace panel.
- Git status badges in the file tree.
- Auto-follow files changed by the currently active agent turn.
- Per-agent and per-turn file change filters.
- Persist recent file-change notifications across browser reloads.

## Open questions

- Should clean open files auto-reload by default, or should this be a user
  setting?
- Should watcher events include content hashes for every file, or should the
  frontend fetch metadata lazily for open tabs only?
- How should non-git workspace roots compute turn-scoped changed files without
  expensive full-tree hashing?
- Should generated/cache directories be globally ignored or configurable per
  workspace?
