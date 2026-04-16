# Bugs & Known Issues

This file tracks reproduced, current issues. Resolved work, speculative refactors,
and cleanup notes do not belong here.

## Active Repo Bugs

Also fixed in the current tree, not re-listed below:

- **Stale height estimate on tab switch causing blank area** — `VirtualizedConversationMessageList` now enters a post-activation measuring phase when it transitions inactive → active (or mounts directly in the active state with messages). The wrapper is hidden via `visibility: hidden` while currently-visible slots report their first `ResizeObserver` measurements, then the completion check writes a final scrollTop and reveals. A 150 ms timeout fallback guarantees the wrapper is never stuck hidden. Scroll-restore on activation now lands in the correct place even when messages arrived while the tab was inactive.
- **Steady-state 1-2 px shake in active session panels** — `handleHeightChange` now rounds `getBoundingClientRect().height` to integer pixels before storage, and all three scrollTop-to-bottom writes (re-pin `useLayoutEffect`, `handleHeightChange` shouldKeepBottom branch, `getAdjustedVirtualizedScrollTopForHeightChange` branch) are wrapped in `Math.abs(current - target) >= 1` no-op guards. Subpixel drift in successive `getBoundingClientRect` reads no longer crosses the 1-pixel commit threshold, and no-op scrollTop writes no longer trigger scroll-event → reflow → ResizeObserver cascades.
- **Mermaid diagram rendering hardcoded a dark theme** — `MermaidDiagram` now receives the active `appearance`, builds the Mermaid config from that value, and serializes initialize/render/reset through `mermaidRenderQueue` so light diagrams can render without leaving Mermaid's global singleton in a stale theme.
- **MarkdownContent line numbers no longer defaulted to line 1** — `MarkdownContent` again defaults `startLineNumber` to `1`, while callers that need unknown source positions can pass `null`. The line-number tests cover the default gutter path again.
- **Rendered Markdown draft lifecycle and save reconciliation** — rendered Markdown diff drafts now use per-segment dirty tracking, flush active DOM drafts before document-content reset and watcher refresh/delete handling, and flush again after pending saves resolve so mid-save typing is preserved.
- **Rendered Markdown section rebuilds with active drafts** — when one rendered Markdown section commits while another section still has a local DOM draft, the editor now commits the active drafts together before the rendered diff can rebuild and remount the downstream section.
- **Rendered Markdown first-render remount** — `EditableRenderedMarkdownSection` now guards `renderResetVersion` with a previous-markdown ref, so initial mounts no longer remount `MarkdownContent`.
- **Markdown diff fenced/list segmentation** — changed fenced code blocks are treated atomically and unordered-list indentation depth stays in the comparison key, so rendered Markdown diffs no longer hide structural list changes or split code fences.
- **Markdown source-link UNC roots** — document-relative Markdown links in UNC workspaces now keep the `\\server\share\` prefix and stay inside the share root.
- **MessageCard appearance memoization** — the memo comparator now includes `appearance`, so existing Markdown/Mermaid cards rerender when light/dark appearance changes.
- **Rendered Markdown regression tests** — rendered Markdown editor tests now cover per-section dirty state, mid-save typing, watcher deletion, downstream draft rebuilds, and line-count-shift editing without `act(...)` warnings.
- **SourcePanel Markdown mode reset coverage** — the reset test now leaves Markdown preview/split mode active before switching to a non-Markdown file, then verifies returning to Markdown starts in code mode.
- **Virtualized first-height measurement and measuring fallback** — first `ResizeObserver` measurements are committed even when they match the estimate, the completion-check schedule is documented, and the 150 ms fallback now re-arms the bottom-pin flag before revealing.
- **Diff preview enrichment note display and persistence** — raw patch fallback now shows `documentEnrichmentNote`, and workspace tab creation preserves note text with a text-specific normalizer instead of treating it as an identifier.
- **Markdown line-number measurement refresh** — line-marker measurement runs before paint again and tracks path/root/link-handler props that affect the measured rendered DOM.
- **Diff scroll ref and Monaco modified scroll restore** — Markdown diff scroll refs now use the stable ref object directly, and Monaco diff scroll restore writes the modified editor side that `getScrollTop()` reads.
- **Unix-only symlink enrichment note branch** — the symlink enrichment classifier branch is now `#[cfg(unix)]`, matching the only platform path that can produce that race.
- **Restored diff preview unmount guard** — pending restored-document fetches now check the mounted-state ref before updating workspace state, so unmounted `App` instances do not accept late restore responses.
- **Stripped diff preview document restore after hydration** — restored Git diff preview tabs are now scanned after workspace layout readiness, not just on initial mount, and request-key guards prevent duplicate document-content restore fetches.
- **Restored diff preview loading persistence** — the workspace persistence sanitizer now removes only empty loading Git diff placeholders, so restored diff tabs with durable diff text survive while their stripped `documentContent` is re-fetched.
- **Oversized Markdown enrichment response coverage** — `load_git_diff_for_request` now has response-level coverage proving oversized Markdown enrichment returns raw diff text, no `documentContent`, and the read-limit `documentEnrichmentNote`.
- **Markdown enrichment internal-error fallback** — unexpected server-side Markdown enrichment failures now degrade to raw diff output with a generic rendered-Markdown-unavailable note instead of failing the whole diff request.
- **Diff tab scroll restore identity guards** — rendered Markdown scroll restore resets on diff-tab identity changes, and the restore retry loop now uses a monotonic token/cancel guard so stale rAF callbacks cannot apply an old restore.
- **Structured Markdown enrichment notes** — Git document read failures now carry an internal `ApiErrorKind`, so `git_diff_document_enrichment_note` no longer depends on free-form error-message substrings for read-limit or symlink-swap cases.
- **Markdown enrichment degraded-path notes** — non-UTF-8 documents, missing Git objects/worktree files, and not-a-regular-file document paths now return raw diff output with a visible rendered-Markdown-unavailable note.
- **Restored diff preview transient loading dedupe** — restored-document scans now skip empty loading Git diff placeholders, preventing a second `/api/git/diff` request while the normal open request is already in flight.
- **Git diff enrichment JSON contract coverage** — degraded Markdown enrichment responses now have serialization coverage for camelCase `documentEnrichmentNote`, omitted `documentContent`, and absence of snake_case response keys.
- **MessageCard interactive callback memoization** — the memo comparator now includes approval, user-input, MCP elicitation, and Codex app request callbacks so handler-only rerenders invoke the latest interactive handlers.
- **Markdown segment-id stability coverage** — the downstream line-count-shift regression now asserts an exact isolated segment and adds a repeated-identical-block fixture so the test proves stable ids for the intended segment instead of passing on a loose substring match.
- **Rendered Markdown committer registry churn** — `DiffPanel` now passes stable ref-forwarded rendered-Markdown callbacks into `MarkdownDiffView`, and coverage asserts that typing in one section does not unregister/re-register all section committers.
- **Pasted rendered-Markdown skip attributes** — editable rendered-Markdown sections now sanitize pasted HTML by removing `data-markdown-*` trust attributes before insertion, and coverage asserts pasted `data-markdown-serialization="skip"` content is still saved.
- **Mermaid render budget** — Markdown rendering now skips Mermaid diagrams over 50,000 source characters or documents with more than 20 Mermaid fences, falling back to source display with regression coverage for both limits.
- **Diff view scroll slots** — changed-only and raw diff views now attach real scroll refs, edit mode exposes a Monaco code-editor scroll handle, and switching among non-default diff views restores their prior offsets.
- **Git diff refresh version reset** — diff refresh versions are now kept monotonic for the browser process lifetime, so closing a diff tab cannot reset the guard while an older fetch for the same request key is still in flight.
- **Restored diff preview App-level coverage** — `App` now has integration coverage for stripped Git diff tabs restored from workspace layout hydration, including request payloads, hydrated document content, propagated enrichment notes, restore failure `loadError`, duplicate-fetch prevention, and late responses after unmount.
- **Rendered Markdown documentContent draft rebase** — active rendered-Markdown DOM drafts now keep the segment and source document from the start of the edit, avoid React reconciliation while dirty, and rebase the saved range when refreshed `documentContent` shifts earlier content.
- **Repeated Markdown diff chunk identity** — rendered Markdown views now reuse previous segment ids across document refreshes by matching content plus nearby structural context, so inserting an identical upstream chunk no longer steals the downstream repeated section's draft identity.

## DiffPanel save adapter drops stale-check and overwrite options

**Severity:** High - rendered Markdown diff saves can bypass stale-file checks and cannot perform an explicit overwrite/save-anyway flow.

`DiffPanel` calls its `onSaveFile` prop with save options such as `baseHash` and `overwrite`, but the `App` adapter currently forwards only the path and content into `handleSourceFileSave`. That means the `/api/file` save request can lose the base hash needed for stale-content conflict detection, and the save-anyway path cannot send `overwrite: true` when the user explicitly chooses to overwrite.

**Current behavior:**
- `DiffPanel` includes `{ baseHash, overwrite }` when saving edited diff content.
- The `App`-level `onSaveFile` adapter drops the third argument before calling `handleSourceFileSave`.
- Backend stale-file conflict detection can be skipped for this path, and conflict recovery cannot send the overwrite flag.

**Proposal:**
- Forward the save options through the `App` adapter into `handleSourceFileSave`.
- Add regression coverage for stale-save conflict detection and save-anyway overwrite from a diff preview.

## Rendered Markdown HTML paste sanitizer allows active markup

**Severity:** High - pasted HTML from the clipboard can be inserted into the TermAl origin with active attributes or unsafe elements intact.

The rendered Markdown paste path parses clipboard `text/html` with `template.innerHTML` and then removes only a narrow set of internal Markdown attributes. That protects the Markdown serialization markers, but it does not strip broader active HTML such as event handlers, dangerous URL protocols, or SVG/embedded content before insertion into the live editable DOM.

**Current behavior:**
- Rendered Markdown paste handling prefers clipboard HTML when available.
- The sanitizer removes `contenteditable`, `aria-hidden`, and `data-markdown-*` attributes.
- Scriptable attributes, unsafe URL protocols, and unsafe HTML/SVG structures are not filtered by an app-owned allowlist.

**Proposal:**
- Convert the rendered Markdown paste path to plain text/Markdown, or sanitize pasted HTML through a strict allowlist before insertion.
- Strip scriptable elements, `on*` handlers, `srcdoc`, unsafe URL protocols, and unsafe embedded/SVG/MathML content.
- Add a regression fixture that combines `data-markdown-serialization` with event handlers and dangerous links.

## Manual Git diff opens bypass request-key generation guard

**Severity:** Medium - an older in-flight Git diff response can still overwrite a newly reopened tab when it comes from the manual open path.

Git diff restore and watcher refreshes use a monotonic request-key version guard, but the manual Git status open path does not currently participate in that same generation check. Closing and reopening the same request key can therefore leave a stale manual/open response racing with the current tab state.

**Current behavior:**
- Restore and file-watch refreshes increment and check `gitDiffPreviewRefreshVersionsRef`.
- `handleOpenGitStatusDiffPreviewTab` fetches a Git diff without bumping or checking that generation.
- A late response from an older open can apply to a newer tab that reused the same request key.

**Proposal:**
- Route every Git diff preview fetch path through one generation helper, including manual Git status opens.
- Check the generation before applying both successful diff responses and errors.
- Add close/reopen regression coverage that resolves an old manual-open request after a newer tab exists.

## Rendered Markdown commit conflicts can drop drafts after partial batch apply

**Severity:** Medium - unresolved rendered Markdown edits can lose their dirty state and conflict notice when another edit in the same batch applies.

Rendered Markdown section commits clear their local draft refs before the parent resolves all ranges. If one section can no longer be mapped but another section in the same batch still applies, the successful partial apply can rebuild state and clear the error while the unresolved draft is no longer tracked as dirty.

**Current behavior:**
- Section-level commit collection clears local draft refs before parent range resolution completes.
- A mixed batch can include one resolvable section and one section that conflicts after document changes.
- Applying the resolvable edit can clear the conflict state and leave the unresolved draft without a visible dirty marker.

**Proposal:**
- Treat rendered Markdown batch commits atomically when any section cannot be mapped.
- Preserve unresolved drafts and the conflict notice until the user resolves or cancels them.
- Add a mixed-batch regression where one rendered section applies and another conflicts.

## Apply-to-disk-version misses active rendered Markdown DOM drafts

**Severity:** Medium - the rebase-to-disk flow can omit edits that are still only present in the active contenteditable DOM.

`handleApplyDiffEditsToDiskVersion` reads the committed edit refs before flushing active rendered Markdown DOM drafts. When the only dirty state is the active DOM draft, or when a DOM draft is newer than the committed refs, the handler can return early or rebase an older edit set and leave the user's latest rendered Markdown change out of the save.

**Current behavior:**
- Rendered Markdown dirty state can come from `hasRenderedDraftActive` even when edit refs are not current.
- The apply-to-disk-version handler reads `editValueRef.current` before flushing active DOM drafts.
- Rebase and save can proceed with stale edit refs, or return before the DOM draft is captured.

**Proposal:**
- Flush rendered Markdown DOM drafts at the start of the apply-to-disk-version handler, then reread the edit refs before rebasing.
- Add regression coverage for a DOM-only active draft and for mixed committed-plus-active rendered Markdown drafts.

## MessageCard interactive callback memo tests cover only approvals

**Severity:** Low - future comparator regressions for non-approval interactive cards may not fail a focused test.

The memo comparator now includes the interactive callback props, but the current regression coverage exercises the approval callback path only. Similar user input, MCP elicitation, and Codex app request handler changes can regress without a targeted test proving the latest callback is invoked after a handler-only rerender.

**Current behavior:**
- Approval callback memoization has a focused regression test.
- User input, MCP elicitation, and Codex app request callbacks rely on broader coverage instead of direct comparator tests.
- A future comparator edit could drop one of those handler props without failing a specific test.

**Proposal:**
- Add focused callback-freshness tests for user input, MCP elicitation, and Codex app request message cards.
- Keep the tests scoped to handler-only rerenders so they fail specifically on memo comparator regressions.

## Committer-registry churn test depends on function source text

**Severity:** Low - the regression test is coupled to implementation details rather than the rendered Markdown behavior it protects.

The committer-registry churn test identifies registered committers by inspecting `Function#toString()` and spies on global `Set.prototype` methods. That can break under harmless refactors, minification-like transforms, or other tests touching `Set`, while still not directly asserting the stable user-visible contract.

**Current behavior:**
- The test distinguishes committers through function source text.
- It patches global `Set.prototype` behavior during the test.
- Future implementation changes can make the test fail for reasons unrelated to section-level committer churn.

**Proposal:**
- Replace source-text detection with a narrow test seam, observable callback identity counter, or DOM-level behavior assertion.
- Avoid global `Set.prototype` instrumentation in favor of local instrumentation around the registry under test.

## Mermaid diagram SVG renders directly in the app DOM

**Severity:** Low - Mermaid output from agent or repository Markdown is inserted into the TermAl origin with `dangerouslySetInnerHTML`.

Mermaid rendering uses `securityLevel: "strict"`, which is a useful baseline, but the returned SVG is still inserted directly into the application DOM. If Mermaid's sanitizer or parser allows an unsafe SVG construct, that construct executes in the same origin as the local TermAl UI and API.

**Current behavior:**
- Mermaid blocks from Markdown are rendered automatically.
- The generated SVG is written with `dangerouslySetInnerHTML`.
- The final SVG is not isolated in a sandboxed frame or sanitized at the app boundary with a local SVG allowlist.

**Proposal:**
- Render Mermaid diagrams inside a sandboxed iframe, or sanitize the returned SVG through a strict app-owned SVG allowlist before insertion.
- Add regression tests with malicious labels, directives, and SVG event attributes.
- Keep showing the source code path for editable Markdown diffs so users can recover from render failures.

## Implementation Tasks

- [ ] P2: Add close/reopen stale Git diff refresh coverage:
  start Git diff refreshes from restore/watch paths and from the manual Git
  status open path, remove the tab while a request is in flight, reopen the
  same request key, resolve the old request late, and assert the old response is
  ignored because every path shares the monotonic request-key generation guard.
- [ ] P2: Add DiffPanel save option forwarding coverage:
  edit a diff preview with a stale `baseHash`, assert the save reports a
  conflict, then exercise the save-anyway path and assert `overwrite: true`
  reaches the file-save request.
- [ ] P2: Add rendered Markdown paste sanitizer security coverage:
  paste clipboard HTML containing `data-markdown-serialization`, event handler
  attributes, unsafe URL protocols, and embedded/SVG content, then assert only
  allowed inert content reaches the editable rendered Markdown DOM.
- [ ] P2: Add rendered Markdown mixed-batch conflict coverage:
  commit two rendered Markdown sections where one range still maps and the
  other conflicts after a document change, then assert no partial apply clears
  the unresolved draft or conflict notice.
- [ ] P2: Add apply-to-disk-version rendered-draft coverage:
  exercise the save-conflict rebase flow with an active contenteditable DOM
  draft only, and again with both committed refs and a newer DOM draft, then
  assert the rebased save includes the latest rendered Markdown edits.
- [ ] P2: Expand MessageCard interactive callback memo coverage:
  add handler-only rerender tests for user input, MCP elicitation, and Codex app
  request cards, matching the approval callback freshness regression.
- [ ] P2: Replace committer-registry churn test implementation-detail instrumentation:
  remove `Function#toString()` committer identification and global
  `Set.prototype` spies from the rendered Markdown registry churn test in favor
  of a local counter or behavior-level assertion.
- [ ] P2: Add Mermaid SVG safety coverage:
  render Mermaid inputs with malicious labels, directives, SVG event attributes,
  `javascript:` or external hrefs, and `foreignObject`, then assert the final
  output is sandboxed or sanitized before it reaches the app DOM.
- [ ] P2: Complete `MermaidDiagram` behavioral coverage:
  cover the remaining gaps: `preserveMermaidSource={true}` keeps the fenced
  source while a diagram renders, the rendered diagram exposes its `role="img"`
  container, the `mermaid-diagram-loading` class is removed after
  `mermaid.render` resolves, and `showSourceOnError={false}` suppresses the
  fallback source. Basic render, light appearance, disabled rendering, and the
  default error fallback are covered.
- [ ] P2: Add fenced-block segmentation edge-case coverage for
  `expandChangedRangeToMarkdownFenceBlocks` / `parseOpeningMarkdownFenceLine`:
  (1) a fence opened with 4+ backticks closed only by a matching-length fence,
  (2) tilde fences (`~~~`) alongside backtick fences, (3) a fence with a
  language followed by an info string, (4) a fenced block adjacent to inline
  code and indented code, and (5) an unclosed fence at end-of-file. Each case
  should assert the segmenter treats the fence as atomic (or explicitly rejects
  it as invalid) instead of splitting opener from body.
- [ ] P2: Cover fenced-block rejection paths in `parseOpeningMarkdownFenceLine`:
  assert inline-code spans (single-backtick runs shorter than 3) do not open a
  fence, assert a fence with a non-language info string (e.g., ``` ``` with
  trailing `{title}`) still matches the fence detector, and assert a fence
  whose language token contains whitespace is parsed as language = first-word
  or rejected consistently.
- [ ] P2: Add a direct unit test for `stripDiffPreviewDocumentContentFromWorkspaceState`:
  feed a workspace state containing a `diffPreview` tab with `documentContent`,
  `documentEnrichmentNote`, `diff`, and `gitDiffRequestKey` populated. Assert
  the output tab has `documentContent` removed but retains
  `documentEnrichmentNote`, `diff`, and `gitDiffRequestKey`. Covers the new
  save/parse pipeline in both `App.tsx` persistence and `workspace-storage.ts`.
- [ ] P2: Replace the brittle `toHaveBeenNthCalledWith(2, ...)` assertion in
  `ui/src/MarkdownContent.test.tsx:106-110` with
  `expect(mermaidInitializeMock).toHaveBeenCalledWith(expect.objectContaining({ theme: "dark" }))`.
  The current assertion pins two successive `mermaid.initialize` calls per
  render (dark init then base reset) and couples the test to the exact
  call order. A future refactor of the mermaid init sequence will break the
  test for reasons unrelated to user-visible behavior.
- [ ] P2: Strengthen the Mermaid diff-view assertion in
  `ui/src/panels/DiffPanel.test.tsx:724`: replace
  `screen.queryByTestId("mermaid-svg") ?? document.querySelector(...)` with
  a single definite assertion, e.g.,
  `expect(document.querySelectorAll(".markdown-diff-rendered-section-added .mermaid-diagram-svg svg").length).toBeGreaterThanOrEqual(1)`.
  The `??` form collapses two assertions into an OR, so a regression that
  drops the testid can still pass if any element matches the fallback
  selector.
- [ ] P2: Move or restore the `SVGElement.prototype.getBBox` /
  `getComputedTextLength` polyfills out of
  `ui/src/panels/DiffPanel.test.tsx:228-243`. The current `beforeEach`
  installs them conditionally on first run and never restores them in an
  `afterEach`, so the patches leak across the entire test file (and any
  test that runs after a DiffPanel test in the same file). Move the
  installation into the shared vitest setup file, or wrap in a proper
  `try`/`finally` with saved originals.
- [ ] Add regression tests for the `VirtualizedConversationMessageList` measuring phase:
  (1) assert the wrapper has `is-measuring-post-activation` on initial mount
  with `isActive=true` + messages; (2) assert the class is removed after all
  visible slots have fired their `ResizeObserver` callback, via the completion
  check path; (3) assert the class is removed after 150 ms via the timeout
  fallback (use `vi.useFakeTimers()` + `vi.advanceTimersByTime(150)`);
  (4) render with `isActive={false}`, rerender with `isActive={true}`, and
  assert the class appears on the inactive → active transition.
- [ ] Add a shake-fix regression test for integer rounding and the `>= 1`
  no-op scrollTop guard in `handleHeightChange`: feed a fractional height
  (e.g., `measuredSlotHeight = 260.7`) through the existing bottom-pin test
  harness and assert the pinned `scrollWrites` target is computed against
  the integer cumulative (not the float). Also add a negative test where
  a measurement implies `target === current scrollTop` and assert
  `scrollWrites` is unchanged after the `ResizeObserver` fires.
- [ ] Update the existing "keeps the bottom pin across successive virtualized
  height commits" test to fire `ResizeObserver` for every visible slot
  before its assertions, so the test doesn't end with `isMeasuringPostActivation`
  still active. Current test survives the measuring-phase refactor only
  because the re-pin effect fires independently — a fragile coincidence
  that could silently break under future refactors.
- [ ] Extract a `pinScrollTopToBottomIfChanged(node)` helper in
  `ui/src/panels/AgentSessionPanel.tsx` and call it from the three sites
  that currently duplicate the `const target = ...; if (Math.abs(...) >= 1) ...`
  block (re-pin `useLayoutEffect`, measuring-phase completion check,
  measuring-phase 150 ms timeout fallback). Optionally also use it in the
  `handleHeightChange` shouldKeepBottom branch. Keeps the no-op guard
  threshold in a single place so future tuning stays consistent.
- [ ] Migrate existing git tests to `init_git_document_test_repo`:
  ten other tests still inline `run_git_test_command(&repo_root, &["init"])` +
  user config without `core.autocrlf=false`, leaving latent Windows CI
  flakiness for any fixture that writes mixed line endings. Start with the
  Markdown-diff tests around `src/tests.rs:21841`, `21908`, `22159`, `22205`,
  `22292`, `22338`, `22585`, `22631`, `26106`, `26223`.
- [ ] Re-query section after Escape cancel in the rendered Markdown regression test:
  the existing "cancels an uncommitted rendered Markdown section edit with
  Escape" test asserts `section.toHaveTextContent("Ready to commit.")` on the
  originally captured reference, which could false-pass if a regression
  remounted the section. Re-query after Escape via
  `document.querySelectorAll` + `find`, and assert `document.contains(section)`.
- [ ] Install jsdom geometry mocks inside `try` blocks:
  both new mock-heavy tests (MarkdownContent ResizeObserver + AgentSessionPanel
  multi-commit pin) install `window.*` overrides BEFORE the `try` block, so a
  future edit that throws during installation could leak globals. Move the
  mock installation inside the `try` so `finally` always runs against the
  saved originals.

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
