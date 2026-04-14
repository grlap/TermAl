# Bugs & Known Issues

This file tracks reproduced, current issues. Resolved work, speculative refactors,
and cleanup notes do not belong here.

## Active Repo Bugs

## Rendered Markdown section draft is silently dropped when save starts mid-edit

**Severity:** High - typing into a rendered Markdown section while a save is in progress produces a user edit that looks committed but is never persisted.

`EditableRenderedMarkdownSection` in `ui/src/panels/DiffPanel.tsx` renders the textarea based solely on its local `isEditing` state, not on `canEdit`. Meanwhile, the parent's `canEditRenderedMarkdown` gate includes `!isSaving`. When a save kicks off (`isSaving` flips true), the parent gate closes, but the child's textarea stays visible because local `isEditing` is still true. The user can keep typing. On blur, `commitDraft` calls `onChange`, which calls `handleRenderedMarkdownSectionChange`, which guards on `!canEditRenderedMarkdown || !markdownPreview || latestFileRef.current.status !== "ready"` and returns early without updating `editValue` or `markdownEditContent`. The draft is silently dropped and the Save button shows "Saved" even though the user's edit was discarded.

In the Ctrl+S-from-textarea path this is impossible because the textarea's `handleTextareaKeyDown` explicitly calls `commitDraft(event.currentTarget.value)` BEFORE `onSave()`. But the same protection doesn't cover any other flow that flips `canEdit` mid-edit: watcher-driven conflict rebase, `documentContent` flipping off, an external click on the Save button while the textarea still has focus, or a concurrent save triggered from another pane.

**Current behavior:**
- `EditableRenderedMarkdownSection` renders a textarea based on local `isEditing` alone.
- The parent's `canEditRenderedMarkdown` is not consulted by the child while editing.
- A mid-edit `isSaving` flip leaves the textarea visible but its commit handler rejects the draft.
- The Save button resolves to "Saved" before the user's edit is persisted.

**Proposal:**
- Have `EditableRenderedMarkdownSection` react to `canEdit` flips via a `useEffect([canEdit])` that calls `commitDraft` before forcing `isEditing = false` when the draft is still accepted.
- Alternatively, surface a visible notice when the commit handler drops a draft (e.g., re-raise `isEditing` and show an "Unable to save — save in progress" chip).
- Alternatively, gate the commit handler so that an in-flight commit always completes regardless of `isSaving`, by latching `canEditRenderedMarkdown` at the moment `commitDraft` is called rather than re-reading it in the parent.
- Add a regression test that starts editing, triggers a save (flipping `isSaving` via a deferred mock), then blurs and verifies the draft is persisted.

## Line-count-shifting commits can unmount an editing Markdown section elsewhere

**Severity:** High - committing an edit in one Markdown section can silently drop an in-progress edit in another section.

`buildFullMarkdownDiffDocumentSegments` in `ui/src/panels/markdown-diff-segments.ts` generates segment React keys that encode positional indices: `added:${segments.length}:${beforeCursor}:${afterCursor}:${afterEnd}`, `normal:${segments.length}:${anchor.beforeIndex}:${anchor.afterIndex}`, etc. When a user commits an edit in section A that adds or removes LINES (e.g., replaces a single-line paragraph with a two-line paragraph), every downstream segment's positional indices shift. Section B's segment id changes, React unmounts the old `EditableRenderedMarkdownSection` for B, and any in-progress local `draftMarkdown` held by B's `useState` is lost. The Plan D comment block explicitly claims "Segments no longer churn during typing ... textarea DOM identity stays stable" — which is true for keystrokes — but the guarantee does NOT extend to line-count-shifting commits in other sections.

The new multi-section commit regression test uses single-line fixtures (`"Section one original." → "Section one revised."`) that preserve the diff's overall shape, so the test passes trivially and does not exercise this scenario.

**Current behavior:**
- Segment React keys use positional indices that shift when line counts change.
- A commit in section A that changes the line count unmounts B's section component.
- B's local `draftMarkdown` state is lost; the textarea remounts with `isEditing=false`.
- The user's B-draft is silently discarded without any save or user notice.

**Proposal:**
- Derive segment React keys from a stable hash of the anchor's `compareText` (or a content hash of the line) rather than positional indices. Stable anchors should have stable keys across structural shifts.
- Alternatively, when a section is `isEditing`, memoize that section's React key on a content-stable identity so line-count shifts upstream don't cause it to unmount.
- Add a regression test that starts editing section A, commits a multi-line edit that shifts line counts, clicks into section B, types, blurs, saves, and asserts B's edit survived.

## Rendered Markdown draft-active flag can leak across documentContent rebases

**Severity:** Medium - `isDirty` can remain true with no corresponding editable buffer after a git refresh invalidates the current rendered segments.

The new dual signal `isDirty = (editValue !== latestFile.content) || hasRenderedDraftActive` in `DiffPanel.tsx` depends on both signals staying correlated. When `documentContent` flips (watcher rebase, git refresh, tab refresh), the effect at `DiffPanel.tsx:281-288` clears `markdownEditContent` if the user has no committed dirty edits (`editValueRef.current === currentFile.content`). But `hasRenderedDraftActive` is NEVER cleared by that effect. If the user had an uncommitted draft in a textarea when the refresh arrived, the segments rebuild, the textarea unmounts (losing the draft), but the flag stays true. `isDirty` reports "unsaved changes" with nothing to save; "Save Markdown" would then write `editValue` — which doesn't reflect the lost draft.

**Current behavior:**
- `hasRenderedDraftActive` is only cleared by `handleRenderedMarkdownSectionChange` (commit path) and `setMarkdownEditContentState` on tab swap.
- The `[documentContent]` reset effect clears `markdownEditContent` but not the flag.
- A watcher rebase mid-edit leaves `isDirty=true` with no real backing buffer.
- Save uses whatever `editValue` happens to be, which may not include the dropped draft.

**Proposal:**
- Include `setHasRenderedDraftActive(false)` in the `[documentContent]` reset effect whenever segments rebuild.
- Alternatively, propagate draft state up from the child section keyed by segment identity so the parent can reconcile dropped drafts with the flag deterministically.
- Add a regression test that types a draft, dispatches a watcher event that rebases `documentContent`, and asserts `isDirty` is false or the draft is preserved via some other mechanism.

## App mount-only diff restore lacks loading indicator and leaks version entries

**Severity:** Medium - restored diff preview tabs show stale content with no refresh indicator, and the refresh-version ref grows unbounded over a session.

The new mount-only effect at `ui/src/App.tsx:3834-3934` re-fetches Git diff preview tabs whose `documentContent` was stripped during persist. Two gaps: (1) the effect does not set `isLoading: true` on the restored tab before issuing `fetchGitDiff`, so the user sees the persisted patch with no "refreshing" indicator for the duration of the round trip; and (2) `gitDiffPreviewRefreshVersionsRef.current` is added-to here and in the watcher-refresh effect, but never purged when a diff tab is closed. Over a long session with many open/close cycles, the ref map grows monotonically.

**Current behavior:**
- The restore effect fires `fetchGitDiff` without touching `isLoading`.
- Users see the stale persisted patch for the round-trip duration with no visible refresh state.
- `gitDiffPreviewRefreshVersionsRef` is never purged for closed tabs.

**Proposal:**
- Wrap the fetch in an optimistic workspace state update that sets `isLoading: true` on the restored tab, then clears it on success/failure.
- Purge version entries from `gitDiffPreviewRefreshVersionsRef` in the diff-tab-close path (which already tracks `gitDiffRequestKey`).
- Optionally abort in-flight mount-time fetches when a tab is closed via `AbortController` to also address the stale-reopen race.

## Git diff enrichment size-limit rejections are invisible to the user

**Severity:** Medium - Markdown files that exceed the 10 MB enrichment cap silently fall back to patch preview with no indication of why rendered Markdown mode is unavailable.

`load_git_diff_for_request` in `src/api.rs` silently swallows both `BAD_REQUEST` and `NOT_FOUND` errors from `load_git_diff_document_content`, logging them via `eprintln!` and degrading `document_content` to `None`. That's correct for path-escape and non-UTF-8 (which represent "this file is not enrichable for safety reasons"), but the same fallback also swallows **legitimate** size-limit rejections (`"exceeds the 10 MB read limit"`) and the `O_NOFOLLOW` `ELOOP` `"changed to a symlink"` error. The diff endpoint returns a raw patch; the user sees "Patch preview" with no idea why rendered Markdown view is unavailable; and the diagnostic only hits stderr, which is rarely visible on Windows.

**Current behavior:**
- `BAD_REQUEST` and `NOT_FOUND` from enrichment are uniformly silently degraded.
- Size-limit rejections produce `eprintln!` warnings only.
- Users of oversized Markdown files have no UI indication of why rendered mode is off.

**Proposal:**
- Add an optional `documentEnrichmentNote: string | null` field to `GitDiffResponse` that the frontend displays as a footnote on the Markdown diff status bar when `documentContent` is absent but the diff is Markdown.
- Alternatively, split internal error types (`GitDocumentLoadError::SizeLimit`, `::NotEnrichable`, `::PathEscape`) and only silently drop the ones that are policy-correct; propagate size-limit as a user-visible note.
- Add a test that creates an oversized Markdown file and asserts the user-facing note is present.

## stripLoadingGitDiffPreviewTabsFromWorkspaceState does double duty under a misleading name

**Severity:** Medium - the function now strips both loading tabs AND documentContent, coupling two unrelated concerns under a name that only advertises the first.

`ui/src/workspace.ts:1852-1891` — `stripLoadingGitDiffPreviewTabsFromWorkspaceState` was originally a scoped cleanup for in-flight loading tabs. It now also calls `stripDiffPreviewDocumentContentFromWorkspaceState` as a side effect, handling the persistence PII scrub. Both `persistWorkspaceLayout` and `parseStoredWorkspaceLayout` rely on this undocumented extra behavior. Any future caller using the function for its original purpose (closing loading tabs) will silently mutate persistable state it had no intention of touching.

**Current behavior:**
- One function handles two unrelated concerns.
- The function name advertises only the loading-tab cleanup.
- Callers depend on the side effect without documentation.

**Proposal:**
- Extract `stripDiffPreviewDocumentContentFromWorkspaceState` as an exported function.
- Have `persistWorkspaceLayout` compose the two explicitly: `stripDiffPreviewDocumentContentFromWorkspaceState(stripLoadingGitDiffPreviewTabsFromWorkspaceState(layout.workspace))`.
- `parseStoredWorkspaceLayout` can skip the loading-tab strip entirely since persisted layouts never contain loading tabs.

## AgentSessionPanel inactive cache flashes wrong content on tab activation

**Severity:** Low - switching back to a cached session briefly paints top-of-window messages before the scroll sync catches up.

`VirtualizedConversationMessageList` in `ui/src/panels/AgentSessionPanel.tsx` no longer returns `null` when inactive, so the conversation DOM stays cached across tab switches. But the scroll-listener `useLayoutEffect` is gated on `isActive`, leaving `viewport` at the default `{height: 600, scrollTop: 0}` while inactive. `visibleRange` therefore picks the FIRST few cards in `windowedMessages`. When the session becomes active, the layout effect fires AFTER the parent's scroll-position restore, so the first paint shows those top-of-window cards before the next frame re-syncs `viewport` to the actual scrollTop. A brief flash is visible.

**Current behavior:**
- Inactive sessions render the first few cards via the default viewport.
- On activation, the first paint shows stale cards before the scroll sync.
- One frame of visual flash is visible.

**Proposal:**
- When inactive, skip the `windowedMessages.slice(...)` render and emit only the wrapper `<div style={{ height: layout.totalHeight }}>`.
- This keeps the height cache for parent scroll-position math, preserves the measured heights cache, avoids the flash, and still passes the existing test asserting `.virtualized-message-list` exists in the inactive page.

## Rendered Markdown preview keyboard handler ignores Space

**Severity:** Low - keyboard-only users pressing Space on a focused Markdown section scroll the document instead of entering edit mode.

`handlePreviewKeyDown` in `EditableRenderedMarkdownSection` handles `Enter` and `F2` but not `Space`. The section has `tabIndex={0}` and is intentionally NOT given `role="button"` (to avoid ARIA issues with interactive descendants), so users can't rely on the role's Space-activation contract. The result is a small accessibility asymmetry: mouse users click to edit; keyboard users press Enter to edit; but Space does nothing useful (it scrolls the page).

**Current behavior:**
- Only `Enter` and `F2` enter edit mode via keyboard.
- Space scrolls the document.
- Keyboard-only users have no Space-activation path.

**Proposal:**
- Add a `Space` branch to `handlePreviewKeyDown` mirroring the `Enter` branch (with `event.preventDefault()` first to suppress the scroll).
- Update the keyboard-activation tests to cover Space.

## Implementation Tasks

- [ ] P2: Add frontend plumbing coverage for Markdown `documentContent` diff tabs:
  open or refresh a Git diff preview with sample staged/index/worktree
  document content and assert the resulting workspace tab preserves enough
  metadata to render the authoritative Markdown side.
- [ ] P2: Add SourcePanel split-mode Markdown coverage:
  switch to `Split`, edit the mocked editor buffer, assert the preview updates
  from the unsaved buffer, and exercise a document link through
  `onOpenSourceLink`.
- [ ] P2: Add MarkdownContent document-relative link tests:
  cover `documentPath` resolution for `./`, `../`, anchors such as
  `api.md#L10`, and Windows-style workspace roots.
- [ ] P2: Extend backend integration tests for Git diff document content across statuses:
  the current tree covers Renamed (staged rename + unstaged edit) and a
  non-Markdown UTF-8 negative case. Still missing: Added, Deleted, and
  Untracked Markdown files in both staged and unstaged sections, plus a
  non-Markdown negative case asserting `document_content.is_none()`. Disable
  `core.autocrlf` in the fixture setup so Windows CI is reliable.
- [ ] P2: Extend the DiffPanel textarea identity test with a focus
  assertion. After each `fireEvent.change` call, assert
  `expect(document.activeElement).toBe(firstTextarea)` so a regression that
  preserves DOM identity but resets focus/selection fails the test. (Plan D
  keeps the textarea mounted without a freeze, but the focus/caret/IME
  preservation itself is still only pinned by DOM identity; add the
  activeElement assertion to harden it further.)
- [ ] P2: Add DiffPanel test coverage for rendered Markdown edits against
  added/removed sections in addition to normal sections: the existing
  "click completes a text selection" test only exercises the normal
  section path, and a maintainer fork of the click handler for a
  rendered-change section would not be caught.
- [ ] P2: Add a DiffPanel regression test for the Escape cancel path in
  `EditableRenderedMarkdownSection`. Enter edit mode, type into the
  textarea, press Escape, and assert `isDirty` is false and the rendered
  preview matches `segment.markdown`. None of the current tests exercise
  the Escape branch.
- [ ] P2: Rewrite or delete the "preserves the edited textarea DOM identity
  across multiple keystrokes" DiffPanel test. Under Plan D the local-draft
  design structurally guarantees the property (drafts never propagate to
  `editValue`, so segments never churn during typing). The test currently
  passes trivially. Either rewrite it to drive segment churn from the
  parent (watcher rebase mid-edit) so it exercises a real regression path,
  or delete it since the property is now guaranteed by construction.
- [ ] P2: Strengthen the AgentSessionPanel inactive-DOM-cached test with a
  reference-identity assertion across activation. The current test
  ("keeps inactive virtualized conversation DOM mounted ...") only asserts
  presence on initial render. Capture the `.virtualized-message-list`
  reference, rerender swapping `activeSession` to the cached session, and
  assert `.toBe()` against the captured node. Alternatively, rename the
  test to match what's actually asserted.
- [ ] P2: Route `src/tests.rs` git test helpers through the `git_command()`
  locale-forcing pattern. `run_git_test_command*` and any other test
  helpers currently call `Command::new("git")` directly without
  `LC_ALL=C`/`LANG=C`, which could flake the new missing-object and
  error-message assertions on CI runners with non-English locales.
- [ ] P2: Finish centralizing `OpenPathOptions` in `ui/src/App.tsx`. The
  `onOpenSourceTab` prop type declarations at two sites plus the
  `handleOpenSourceTab` implementation still inline the
  `{ line?, column?, openInNewTab? }` shape instead of importing
  `OpenPathOptions` from `ui/src/api.ts`. They're structurally identical
  today, but the centralization isn't fully threaded through App.tsx.
- [ ] P2: Add a negative test for Markdown mode visibility:
  render a non-Markdown diff (`.ts`/`.rs`) and assert
  `screen.queryByRole('button', { name: /Markdown/ })` is null.
- [ ] P2: Expand SourcePanel Markdown mode tests beyond preview:
  cover split-mode rendering, Code→Preview→Split→Code round-trip, and the
  mode reset when switching from a Markdown file to a non-Markdown file.
- [ ] P2: Add a test for the MarkdownLinkContext anti-nesting behavior:
  render `` [prefix `lib/models/foo.rs` suffix](https://example.com) `` and
  assert `document.querySelectorAll('a a').length === 0` while inline code
  still renders.
- [ ] P2: Strengthen CRLF rendered Markdown test assertions:
  assert that CR characters are absent from the rendered output and that
  matched lines render exactly once, and add a test where both sides use
  CRLF line endings.
- [ ] P2: Add a render-level test for the round 9 multi-commit pin
  survival in `ui/src/panels/AgentSessionPanel.tsx`. The 11 boundary
  unit tests pin the constants `4` and `72`, but the actual round 8
  Medium bug fix mechanism — that the `useLayoutEffect` no longer
  clears `shouldKeepBottomAfterLayoutRef.current`, so the re-pin
  survives a `setLayoutVersion` → follow-up-commit transition — is
  not exercised by any test. A future refactor that moves the clear
  back into the re-pinning effect (or into the same-commit branch of
  `handleHeightChange`) would leave the 11 boundary tests passing
  while the pin silently regresses. Mount
  `VirtualizedConversationMessageList` (or export it for the test),
  stub `getBoundingClientRect` / jsdom geometry to drive
  `handleHeightChange` with an initial "near bottom" node, fire a
  growing height for a specific message, spy on `node.scrollTop`
  assignments, and assert two successive growth commits produce two
  re-pin writes (not just one). Alternatively, use an integration
  test through `AgentSessionPanel` that dispatches state updates with
  growing message measurements and asserts the viewport remains
  pinned across multiple commits.
- [ ] P2: Tighten `keeps the new-response button scroll correction
  alive for the explicit minAttempts floor` in `ui/src/App.test.tsx`
  to actually pin the EXPLICIT `minAttempts: 8`. The production call
  uses `maxAttempts: 60, minAttempts: 8`, but
  `resolveSettledScrollMinimumAttempts(60)` already returns 8 via the
  default `maxAttempts > 12 ? 8 : 4` branch — so dropping the
  explicit `minAttempts: 8` from the production call would still
  pass. Either use `maxAttempts: 12, minAttempts: 8` in a second
  integration assertion (so the default floor would drop to 4 and
  only the explicit arg keeps it at 8), or drop the integration test
  and rely on the unit tests at 10727-10734 alone — they already pin
  `resolveSettledScrollMinimumAttempts` cleanly and the integration
  harness adds cost without catching the regression the title claims.
- [ ] P2: Decide the fate of `isScrollContainerAtBottom` in
  `ui/src/panels/AgentSessionPanel.tsx:3140`. The function is exported
  and has 4 boundary unit tests but zero production callers — the
  only references inside `AgentSessionPanel.tsx` are two comments
  describing the round 8 pre-fix behavior. Either delete the helper
  and its tests (the historical-bug explanation can stay as a
  comment), or add a brief comment on the helper itself explaining
  why a dead export is intentionally retained as a documentation
  anchor. Three reviewers flagged this independently.
- [ ] P2: Optionally tree-shake `setAppTestHooksForTests` out of
  production builds in `ui/src/App.tsx`. The hook is currently safe
  (observer-only callback receiving hard-coded `"resolve" | "reject"`
  label arguments with ignored return values, gated on optional
  chaining behind the `isMountedRef.current` guard, cleared in
  `afterEach`), but it ships unconditionally in the production
  bundle. A future hook field that receives richer arguments
  (`StateResponse`, session text, draft attachments) would start
  leaking sensitive data into any code running in the page through
  the same export pattern. Either wrap the export in
  `if (import.meta.env.MODE === "test")` / a separate `App.testing.ts`
  entry, or add a code comment requiring all current and future
  fields to accept only non-sensitive label arguments.
- [ ] P2: Export the `AppTestHooks` type in `ui/src/App.tsx:893`. The
  type is currently a local declaration that
  `setAppTestHooksForTests` annotates, forcing test consumers to
  either duplicate the shape inline or use
  `Parameters<typeof setAppTestHooksForTests>[0]`. Free type-surface
  improvement with no runtime cost.
- [ ] P2: Add `expect(resolveSettledScrollMinimumAttempts(0)).toBe(0)`
  to `ui/src/App.test.tsx:10727-10734`. With `maxAttempts = 0`, the
  default branch yields 4 but `Math.min(4, 0) = 0`, so the helper
  returns 0 — which would bypass the entire attempt-count floor. Not
  used today, but a minimal `max=0` edge assertion would pin the
  invariant so a future refactor that special-cased the floor (e.g.
  `Math.max(1, ...)`) would have a test to flip.

## Known Design Limitations

These are deliberate design tradeoffs, not bugs, but are recorded here so
they stay visible to future contributors and can be revisited if the
tradeoff space changes.

### Untracked Git diff previews have a 10 MB read cap

Untracked file diffs use the same `MAX_FILE_CONTENT_BYTES` ceiling as rendered
Markdown document reads. Files above that cap return a read-limit error instead
of building an unbounded synthetic `+` diff in memory.

**Accepted tradeoff.** This is a deliberate defense against large accidental
untracked files such as logs or generated artifacts. The UI records the backend
error on the pending diff tab instead of crashing, and normal staged/tracked Git
diffs still come from Git itself.

### Terminal commands have no production watchdog

Terminal commands intentionally run without a production timeout. The terminal
panel is used for long-lived foreground workflows such as `flutter run`, dev
servers, watch tasks, and REPL-like tools, so a watchdog would terminate
commands users expect to keep alive.

**Consequence:** a running command holds its terminal concurrency permit until
it exits or the stream is disconnected. The streamed local path now observes
SSE disconnects and kills the local process tree, but ordinary JSON terminal
runs and remote command lifetime are still governed by the command/backend
itself rather than a TermAl watchdog.

**Mitigations already in place:**
- Local and remote terminal commands have separate concurrency caps.
- Captured output and live stream buffers are bounded.
- The no-timeout behavior is documented at the local and remote terminal
  launch sites in `src/api.rs`.

### Remote terminal stream worker thread can stay parked on a stalled remote

`InterruptibleRemoteStreamReader::spawn` in `src/remote.rs` wraps the
blocking remote HTTP body read in a dedicated OS thread that pushes chunks
into an `mpsc::sync_channel(1)`. The main forwarding loop reads from the
channel with `recv_timeout(10ms)` and observes the cancellation flag
between polls, so the user-visible "4-in-flight remote permit" path is
correctly released on client disconnect — the forwarder returns
`terminal stream client disconnected`, drops its end of the channel,
releases the semaphore permit, and exits. The spawned reader thread,
however, is still parked inside `source.read(&mut scratch)` until the
reqwest body read finally returns (a byte arrives, the socket closes, or
an error fires). Because the backend intentionally builds its
`BlockingHttpClient` without a body read timeout (so legitimate long
streams can keep producing output), a remote that holds its socket open
without emitting bytes will pin the reader thread and its TCP socket
until the remote finally closes the connection.

**Consequence:** repeated client-disconnect cycles against a stalled
remote accumulate detached reader threads + sockets, one per disconnect,
until the remote eventually closes its side. Each thread holds a
~2-8 MB stack and a single TCP connection. The bound is set by the
remote's own keepalive / TCP timeout behaviour, not by TermAl.

**Accepted tradeoff.** The three real fixes — setting a per-read body
timeout on reqwest (not supported by the blocking API), moving the read
onto an async `tokio::select!` with a cancellation future (a large
rewrite of the remote bridge), or manually `dup`ing the raw socket fd
and closing it from outside (unsafe, platform-specific, bypasses reqwest
encapsulation) — are all strictly larger than the bounded dormant-thread
cost. This mirrors the existing "Unix terminal clean-exit cleanup is a
no-op" limitation below: both are bounded native-thread leaks that wait
on an external event (grandchild pipe close, remote socket close), and
both are left as-is because the cleanup machinery needed to plug them
would be more invasive than the leak they prevent.

**Mitigations already in place:**
- The forwarder returns promptly on cancellation via the adapter's
  `recv_timeout` poll, so the user-visible semaphore permit is released
  immediately and new commands are not blocked by dormant reader threads.
- `read_remote_stream_response` re-checks the cancellation flag between
  reads, so a reader thread that finally gets a byte from the remote
  exits on the next loop iteration rather than continuing to buffer.
- `InterruptibleRemoteStreamReader::spawn_unblocks_on_cancellation` and
  `interruptible_remote_stream_reader_observes_cancellation_between_recv_timeouts`
  pin the two sides of the contract so a future edit that breaks either
  the adapter poll or the spawn path fails a specific regression test.

### Unix terminal clean-exit cleanup is a no-op

On Unix, `TerminalProcessTree::cleanup_after_shell_exit` is intentionally
a no-op on the success path. Once `wait_for_shared_child_exit_timeout` has
reaped the shell, the kernel is free to recycle the shell's PID (and
therefore its process group id), so calling `libc::killpg(process.id(),
SIGKILL)` would race PID reuse and could SIGKILL an unrelated local
process group. Rust's stdlib `Child::send_signal` guards against this
same hazard by early-returning once the child has been reaped.

**Consequence:** a command like `sleep 999 & echo done` will return
successfully, release its terminal-command permit, and leave the
backgrounded `sleep` running outside TermAl's accounting. The backgrounded
grandchild re-parents to init (PID 1), so it is owned by the OS rather
than leaked in TermAl itself, but it is not bounded by the terminal
command timeout or the 429 semaphore.

**Mitigations already in place:**
- On Windows the path is completely covered: the Job Object with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` terminates every process assigned
  to it when the shell exits, so backgrounded grandchildren are killed.
- On Unix, the per-stream reader-join timeout
  (`TERMINAL_OUTPUT_READER_JOIN_TIMEOUT`, 5s) bounds how long the terminal
  command waits for each stdio reader to finish even if a backgrounded
  grandchild still holds the inherited stdout/stderr pipes.
  `run_terminal_shell_command_with_timeout` joins stdout and stderr
  **sequentially**, so the pathological success-path reader-join phase
  is up to ~10s (5s per stream), not 5s. That phase runs *after* the
  child wait returns, so on the local path the total worst-case wall
  clock is `TERMINAL_COMMAND_TIMEOUT` (60s child wait) + ~10s (reader
  join), or approximately 70s. On the remote-proxy path the same ~70s
  inner budget fits inside the 90s `REMOTE_TERMINAL_COMMAND_TIMEOUT`
  envelope with ~20s of slack. Tuning `TERMINAL_OUTPUT_READER_JOIN_TIMEOUT`
  should account for both budgets and the 2x sequential multiplier.
- Reader threads write into an `Arc<Mutex<TerminalOutputBuffer>>` shared
  with the main thread (see `read_capped_terminal_output_into` and
  `join_terminal_output_reader` in `src/api.rs`). Each reader signals
  completion via a `sync_channel(1)` so `join_terminal_output_reader`
  blocks in `recv_timeout` instead of polling -- the happy-path wake is
  event-driven, not tick-driven. On a reader-join timeout, the main
  thread snapshots whatever prefix the reader already accumulated and
  returns it marked `output_truncated = true`. Without the shared
  buffer, the main thread dropped the `JoinHandle` and returned
  `String::new()`, so any `echo done` output that had already been
  buffered was silently discarded even though the foreground shell
  produced it. The detached reader thread still runs until the
  backgrounded grandchild closes its inherited pipe end, but no data
  captured up to the timeout is lost.
- The timeout path still calls `killpg` (reserved for the pre-reap window,
  where `process.id()` is guaranteed to still refer to our process), with
  an additional `try_wait` defense against a narrower race where
  `wait_for_shared_child_exit_timeout`'s detached waiter thread reaps the
  shell between `recv_timeout` returning and the kill running.

**Residual cost:** the detached reader thread stays alive until the
backgrounded grandchild closes its inherited pipe end (i.e. until it
exits). Repeating long-lived background commands can therefore accumulate
native threads for as long as each grandchild survives. This is a bounded
leak -- the thread exits when its target exits, no data captured up to
the timeout is lost, and on Windows the Job Object prevents the scenario
entirely -- and closing it would require platform-specific pipe-
interruption primitives that are not currently worth the added
complexity.

**Possible future strategies** (none currently implemented because the
tradeoff isn't obviously worth the complexity):
- Linux-only: use `pidfd_open` for a stable process handle and
  `pidfd_send_signal` for race-free kills. Doesn't help macOS, and doesn't
  give us a group handle anyway.
- Install `PR_SET_CHILD_SUBREAPER` on the TermAl process so backgrounded
  grandchildren re-parent to TermAl rather than init, then track them
  explicitly. Linux-only and complicates the whole process model.
- Use cgroups v2 on Linux for a process-group handle that isn't tied to a
  recyclable PID. Requires root or unified cgroups and still Linux-only.
- Unix-only: `dup` the pipe fds before moving them into the reader
  threads, keep the duplicates in the main thread, and `close` them on
  reader-join timeout. This would force the blocking `read` to return
  `EBADF` / `EOF` and let the detached thread unwind immediately,
  eliminating the thread-accumulation residual cost. Adds platform-
  specific code and nontrivial fd plumbing.

### Terminal 429 peek/resolve race drifts local-vs-remote counters

`run_terminal_command` calls `state.terminal_request_is_remote(...)`
under the state lock to decide which permit (local or remote) to
acquire, then drops the lock, acquires the chosen permit, and only later
resolves the full scope via `remote_scope_for_request` inside
`run_blocking_api`. If a caller's `projectId` is local at the peek but
its remote binding flips between the two calls, the local permit is
consumed for a request that then fails deep inside the blocking task --
or vice versa. Both sides fail closed (mismatches return safely as
`ApiError`), but the 429 counters can transiently diverge from what is
actually in flight on each budget.

**Accepted tradeoff.** Closing this race would require snapshotting the
full resolution (scope, not just a boolean) before acquiring the permit,
which means running `ensure_remote_project_binding` -- a blocking
`reqwest::send` on the first-time-bind path -- on the async worker
thread. The round-99 refactor moved that call onto the blocking pool for
async-safety, and reintroducing it on the async worker is a strictly
worse tradeoff than the rare 429-counter asymmetry it would fix. The
race is documented in a large inline comment in `run_terminal_command`
right above the `terminal_request_is_remote` peek so future readers do
not chase the asymmetric counters as a bug.

### Windows `resume_terminal_process_threads` snapshots every system thread

`resume_terminal_process_threads` on Windows calls
`CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0)` on every terminal
command, which enumerates every thread on the entire system (typically
2-5k on a dev workstation, more on busy servers). The function then
iterates to find the subset belonging to the child process.

**Accepted tradeoff.** The `TH32CS_SNAPTHREAD` snapshot kind does not
accept a process-id filter (the `pid` parameter is only honored by the
module snapshot kinds), so there is no cheap way to narrow the snapshot
at the Win32 API layer. Capturing the primary thread handle directly
from `CreateProcess` via `PROCESS_INFORMATION.hThread` and calling
`ResumeThread` on just that one handle would work, but it requires
bypassing `std::process::Child`'s encapsulation -- either with a
crate-level extension trait or a direct `CreateProcess` call that
mirrors stdlib's stdio plumbing. That is a substantially larger
refactor than the roughly 10 microseconds the snapshot costs in practice,
so the current implementation is left as-is with a prominent comment
documenting the reason.

## Known External Limitations

No currently tracked external limitations require additional TermAl changes.

Windows-specific upstream caveats are handled in-product:
- TermAl forces Gemini ACP sessions to use `tools.shell.enableInteractiveShell = false`
  through a TermAl-managed system settings override on Windows.
- TermAl surfaces WSL guidance before starting Codex on Windows because upstream
  PowerShell parser failures still originate inside Codex itself.
