# Bugs & Known Issues

This file tracks reproduced, current issues. Resolved work, speculative refactors,
and cleanup notes do not belong here.

## Active Repo Bugs

Also fixed in the current tree: the `ui/src/preferences/` directory is
now staged (both `SettingsTabBar.tsx` and `preferences-tabs.ts`) so the
tab-bar extraction no longer leaves App.tsx importing untracked files;
a clean checkout will type-check.

Also fixed in the current tree: four `App.test.tsx` preference tests
(at lines 15177, 15347, 15830, 15891) now query the shortened tab
labels — `Codex`, `Claude`, `Editor & UI` — instead of the
pre-shortening names, so the `getByRole("tab", { name: ... })` lookups
succeed.

Also fixed in the current tree: the provenance header in
`ui/src/control-surface-state.ts` no longer claims its exports are
visible through an `App.tsx` re-export. The last paragraph now
correctly states that consumers (including `App.test.tsx`) import
directly from the new module, matching what the code actually does.

Also fixed in the current tree: the dead `export { ... } from
"..."` re-export blocks that `SessionPaneView.tsx` and
`WorkspaceNodeView.tsx` had cloned verbatim from App.tsx's
surface during the extractions are gone. No test or consumer
imported those symbols through the new modules; `App.tsx` already
re-exports the same names from their true-source modules.
Type-check stays clean.

Also fixed in the current tree: the `ConnectionRetryCard`
null-`attemptLabel` fallback path now has a Vitest assertion.
`MessageCard.test.tsx` gained a third case that renders a notice
with no "(attempt N of M)" suffix on both `isLatestAssistantMessage`
branches, asserts the attempt chip is absent on each, and pins
the generic past-tense fallback copy verbatim.

Also fixed in the current tree: the pre-existing `App.test.tsx`
"ignores stale manual Git diff responses after reopening the
same request key" failure (present since commit `00f3390` when
the test was first introduced) is green. Root cause was a
deterministic race: `handleOpenGitStatusDiffPreviewTab` scheduled
its own `fetchGitDiff` and wrote the result back with
`documentContent: null`; the restore-from-persisted-layout
useEffect then treated the manually-loaded tab as a stub needing
another fetch and fired a duplicate request. Fix in
`App.tsx:6020` marks the `requestKey` as attempted at the top of
`handleOpenGitStatusDiffPreviewTab` so the restore useEffect
skips it. Full vitest suite is now 46 files / 893 tests, 0
failures.

## Dead `lazy(MonacoCodeEditor)` handle in `DiffPanel.tsx`

**Severity:** Low - `ui/src/panels/DiffPanel.tsx:122-124` still declares `const MonacoCodeEditor = lazy(() => import("../MonacoCodeEditor")...)` after commit `3d89d02` moved the `renderEditFileView` JSX out to `./render-edit-file-view.tsx`. The DiffPanel body now only references the type aliases (`MonacoCodeEditorHandle`, `MonacoCodeEditorStatus`) and the ref; nothing uses the lazy wrapper as JSX. The new module (`render-edit-file-view.tsx`) has its own identical lazy handle, and its file header explicitly documents the paired-lazy choice. But DiffPanel's copy is now inert — tree-shakable in the bundle, but a red herring in the source.

**Current behavior:**
- DiffPanel declares `const MonacoCodeEditor = lazy(...)` at lines 122-124.
- No JSX reference to that symbol remains in DiffPanel.tsx.
- The sibling `render-edit-file-view.tsx` declares a separate `lazy()` handle that is the one actually rendered.

**Proposal:**
- Delete `const MonacoCodeEditor = lazy(...)` from DiffPanel.tsx.
- If `lazy` / `Suspense` become unused in DiffPanel after the deletion, drop them from the `react` import on line 1 as well.

## Unused `type ContentRebaseResult` import in `SourcePanel.tsx`

**Severity:** Low - `ui/src/panels/SourcePanel.tsx:15` imports `type ContentRebaseResult` from `./content-rebase`, but after the rebase-helpers extraction only `rebaseContentOntoDisk` is referenced at runtime. The type alias moved with the function but the import was not pruned.

**Current behavior:**
- `import { rebaseContentOntoDisk, type ContentRebaseResult } from "./content-rebase";` at SourcePanel.tsx:14-17.
- `ContentRebaseResult` is never referenced in SourcePanel.tsx.
- `tsc --noEmit` passes because `noUnusedLocals` is not enabled in the tsconfig.

**Proposal:**
- Drop `type ContentRebaseResult` from the import; keep `rebaseContentOntoDisk`.

## `LatestFileState.contentHash` inconsistent between creators

**Severity:** Low - `ui/src/panels/diff-latest-file-state.ts:43` declares `contentHash?: string | null;` (optional). `toLatestFileState` always writes `contentHash: response.contentHash ?? null`. `createInitialLatestFileState` omits the field entirely. The two creation paths therefore disagree on whether `contentHash === undefined` vs `=== null` for a just-opened tab. This is byte-identical to the pre-extraction behaviour in DiffPanel.tsx — inherited, not introduced by the split — but the inconsistency is now isolated in one focused module, making it a clean one-line fix.

**Current behavior:**
- Idle / loading state has `contentHash` absent.
- Ready state has `contentHash: string | null`.
- Any caller that distinguishes `undefined` vs `null` would see different values for the two origin paths.

**Proposal:**
- Set `contentHash: null` in `createInitialLatestFileState` for both the idle and loading branches, or tighten the type to `contentHash: string | null` (non-optional) so the omission is a compile error.

## `rendered-diff-view.tsx` header comment doesn't match the code

**Severity:** Note - the "What this file does NOT own" block in `ui/src/panels/rendered-diff-view.tsx` claims the diff panel and source panel "compose their synthetic Markdown slightly differently (the source pane joins regions with a plain separator; the diff pane shares the same region-header format but stays a distinct consumer)." In practice both modules (`composeRenderedDiffMarkdown` in `rendered-diff-view.tsx`, `composeRendererPreviewMarkdown` in `source-renderer-preview.tsx`) join regions with `"\n\n"` and emit the same `**Lines N–M**` header followed by an identical fence body. The implementations are near-identical.

**Current behavior:**
- Header comment describes a distinction the code does not make.
- A future reader could rely on the note and introduce a real behavioural divergence, or spend time hunting for the described difference.

**Proposal:**
- Update the header note to accurately describe the near-duplicated behaviour (e.g., "Kept separate from `./source-renderer-preview`'s near-identical `composeRendererPreviewMarkdown` / `composeRendererPreviewRegion` because the two panels were not unified in the split batch; consider consolidating into a shared helper in a future pass.").
- Or: extract a shared `panels/synthetic-rendered-markdown.ts` helper that both consumers import.

## `markdown-diff-edit-pipeline` paste sanitizer has no direct unit tests

**Severity:** Medium - `ui/src/panels/markdown-diff-edit-pipeline.ts::isSafePastedMarkdownHref` (line 216) and `ui/src/panels/markdown-diff-edit-pipeline.ts::sanitizePastedMarkdownFragment` (line 161) are the security boundary for paste-into-Markdown. Neither has any direct Vitest coverage, and no integration test in `DiffPanel.test.tsx` exercises these two functions end-to-end.

`isSafePastedMarkdownHref` has four branches (empty-string rejection, drive-letter allowlist, no-colon relative-path allowance, and the `http` / `https` / `mailto` protocol allowlist) plus a control-character stripper (`/[\u0000-\u001F\u007F\s]+/g`) that runs before protocol extraction. A regression that flipped `protocol === "http"` to include `"javascript"`, or weakened the control-char filter (so `java\u0000script:` slipped past the protocol check), would ship silently — nothing else in the repo references these symbols.

`sanitizePastedMarkdownFragment` has three guards (HTML namespace, 23-element drop set, 32-element allow set) and an attribute-stripping pass that keeps only `href` on anchors (gated by `isSafePastedMarkdownHref`) and `class` on code (gated by `normalizePastedMarkdownCodeClass`). A set membership regression (e.g., accidentally removing `button` from the drop set, or adding `svg` to the allow set) would ship silently.

**Current behavior:**
- Both helpers exist as extracted pure functions in `./markdown-diff-edit-pipeline`.
- Neither has a `markdown-diff-edit-pipeline.test.ts`.
- `DiffPanel.test.tsx` does not exercise a paste flow that would stress the sanitizer.

**Proposal:**
- Add `ui/src/panels/markdown-diff-edit-pipeline.test.ts` with table-driven cases for `isSafePastedMarkdownHref`: `javascript:`, `data:`, `vbscript:`, `JAVASCRIPT:`, `java\u0000script:`, `  javascript:` (leading whitespace), `C:\foo`, `c:/foo`, `http://a`, `https://a`, `mailto:a@b`, `./relative`, `#anchor`, empty string, whitespace-only.
- Pair with sanitizer tests that take a `template` fragment containing `<a onclick="x">`, `<iframe>`, `<svg>`, `<script>`, `<button>`, `<code class="language-py bad">`, and assert the post-sanitize DOM shape: removed elements gone, unwanted attributes stripped, `language-py` kept on `<code>`.
- Include a regression guard on the `<template>`-content sanitize-before-insert ordering (verify no `<script>` fetch fires before `sanitizePastedMarkdownFragment` runs).

## Missing error boundary around portal `render()` in MonacoCodeEditor

**Severity:** Medium - `ui/src/MonacoCodeEditor.tsx:651-657` invokes `host.zone.render()` inline with no error boundary. The `render` callback in `SourcePanel` returns `<MarkdownContent>`, which in turn runs Mermaid/KaTeX detection. If anything inside that subtree throws during render (a malformed fence, a KaTeX parse failure that slips past `throwOnError: false`, a Mermaid render-time exception), the entire `MonacoCodeEditor` component errors and React unmounts the Monaco editor along with the inline zones — losing whatever the user had in their buffer.

**Current behavior:**
- `createPortal(host.zone.render(), host.node, host.id)` runs unprotected.
- A single bad fence in a file can take down the whole editor.
- Save-buffer loss is user-visible and irrecoverable without the autosave mechanism (which doesn't exist in Phase 1).

**Proposal:**
- Wrap each portal's children in a small error boundary component, e.g. `<InlineZoneErrorBoundary>{host.zone.render()}</InlineZoneErrorBoundary>`.
- Fallback UI: "Diagram failed to render — view the source below for details." The source stays visible in Monaco regardless.
- Add a Vitest case that passes a `render` callback throwing synchronously and asserts the editor remains mounted.

## `setInlineZoneHostState` writes fresh state on every keystroke

**Severity:** Medium - `ui/src/MonacoCodeEditor.tsx:354-361` calls `setInlineZoneHostState` with a fresh array on every `inlineZones` prop change. Since `SourcePanel` rebuilds `inlineZones` on every keystroke (the `renderableRegions` memo depends on `editorValue`), the zone-host state is written unconditionally even when the zone set is structurally unchanged.

This cascades through the ResizeObserver effect (whose dep is `inlineZoneHostState`), disconnecting and reconnecting the observer on every keystroke. The diagram DOM survives via stable zone ids + portal key, so correctness is intact — but the per-keystroke work is O(zones) observer setup + O(zones) re-observe calls.

**Current behavior:**
- `useEffect([inlineZones])` writes `setInlineZoneHostState(fresh-array)` every time `inlineZones` identity changes (every keystroke).
- The `[inlineZoneHostState]` observer effect disconnects and re-creates the ResizeObserver on every re-render.
- Symptom-free today; performance degrades linearly with zone count.

**Proposal:**
- Shallow-compare the new zone set against the current state before calling `setInlineZoneHostState` (same ids in same order + same inner-node refs → no-op).
- Or move the ResizeObserver setup into the zone-registry effect, observing/unobserving specific nodes incrementally rather than recreating the observer.

## Rendered-diff fallback uses worktree content for staged diffs

**Severity:** Medium - `ui/src/panels/DiffPanel.tsx:556-567` defines `renderedDiffAfterContent` as `documentContent?.after?.content ?? latestFile.content`. For a **staged** diff when `documentContent` is missing (large file, unsupported binary, read error), the fallback uses `latestFile.content` — which is always the worktree (from `fetchFile` on the current working file). That can contain unstaged edits unrelated to the staged diff, so the Rendered view misrepresents the index side.

The UI labels this "Patch-only rendering: best-effort approximation" so reviewers are warned, but the sibling `buildMarkdownDiffPreview` fallback for Markdown uses `preview.modifiedText` (derived from the patch itself), which is more faithful. The Rendered view's fallback is less accurate than its label admits.

**Current behavior:**
- Staged diff + missing `documentContent` + worktree carries unstaged edits → Rendered view shows the worktree (unstaged) version, not the index.
- "Patch-only" label is correct but understates the divergence.
- Test at `DiffPanel.test.tsx:430-466` only exercises the unstaged path where worktree == "after side" anyway, so the bug isn't caught by coverage.

**Proposal:**
- Derive the rendered-view fallback from `buildDiffPreviewModel(diff, changeType).modifiedText` (same source the Markdown fallback uses) so staged/unstaged side semantics are preserved by construction.
- OR suppress the "Rendered" button entirely when `documentContent` is missing AND `gitSectionId === "staged"`.
- Add a Vitest case: staged diff, no `documentContent`, worktree contains different content than the patch's after-side → the Rendered view matches the patch, not the worktree.

## `remote_sync.rs` diagram + prose had three factual errors (now fixed)

**Severity:** Medium - not a live bug anymore but worth recording for posterity. The Mermaid flowchart I added to `sync_remote_state_inner` in commit 10a2515 claimed the function "upserts remote projects" (it doesn't — project state is managed elsewhere), placed `retain_sessions` AFTER the orchestrator sync (the real order runs retain BEFORE session updates), and attributed `note_remote_applied_revision` to this function (callers do it — the revision gate lives in the wrapper). The original prose at the top of the function had the same "upserts every remote project" error, so the diagram just codified an existing mistake more visibly.

Fixed in this review cycle: the diagram now shows the correct broad-sync flow (Capture → BuildMap read-only → Retain → SessionUpdates → OrchestratorSync → Restore-on-failure), the focused-path capture timing (AFTER session updates), and drops the `NoteRevision` node entirely. The prose paragraphs were rewritten to match the mutation contract that's enforced by `RemoteSyncRollback`.

**Current behavior:**
- Diagram and prose now match the code.
- Mutation contract comment (lines 130-140 of `remote_sync.rs`) continues to enumerate exactly the fields `RemoteSyncRollback::capture` covers, and the diagram no longer contradicts it.

## Inline-zone id stability not exercised by tests

**Severity:** Medium - the mock in `ui/src/panels/SourcePanel.test.tsx:32-34` exposes a `data-inline-zone-ids` attribute (comma-joined zone ids) but no test asserts against it. The whole point of stable ids is that the portal DOM node survives keystrokes outside the fence — Mermaid iframe stays initialized, KaTeX output isn't re-parsed. But nothing pins that contract. A regression that re-hashes the id on every call would pass all current tests.

**Current behavior:**
- Mock surfaces the attribute; tests assert `data-inline-zone-count` and `data-inline-zone-first-after-line` only.
- Particularly relevant because `detectWholeFileMermaidRegion` hashes the entire content (`mermaid-file:${quickHash(context.content)}`), so `.mmd` file ids WILL change on edits — the stability contract only applies to Markdown files with fence-scoped ids.

**Proposal:**
- Add a Vitest case: capture `data-inline-zone-ids` before an edit, type a line above the fence (outside all regions) in a Markdown file with a Mermaid fence, assert the first zone's id is unchanged.
- Document the `.mmd` file exception explicitly (whole-file regions hash the whole content, so ids do shift on any edit).

## "Patch-only absence" not asserted on the complete-document path

**Severity:** Medium - `ui/src/panels/DiffPanel.test.tsx:370-428` asserts the "Rendered" button appears when `documentContent.isCompleteDocument: true`, but never asserts the Patch-only banner is **absent**. The paired test at line 465 asserts presence when `documentContent` is missing. A regression that flipped the gating logic (e.g., rendering the banner unconditionally) would pass both tests.

**Current behavior:**
- Only positive assertion is "Patch-only" appears when `documentContent` is missing.
- No negative assertion for the complete-document path.

**Proposal:**
- In the complete-document test at line 370, after `await clickAndSettle(renderedButton)`, add: `expect(screen.queryByText(/Patch-only rendering/i)).not.toBeInTheDocument();`.

## Math-counter boundary cases missing

**Severity:** Medium - `ui/src/source-renderers.test.ts` covers `countMathExpressions` for inline math and single-line `$$...$$`, but misses three boundary scenarios that the production code specifically handles:

- Two consecutive multi-line `$$...$$` blocks separated by a blank line. The state-machine toggle at `source-renderers.ts:133-139` is subtle — a trailing `$$` that closes a block followed by a new opening `$$` must count as 2. No test pins this.
- `$$` inside a fenced code block. The existing `does not count $ inside fenced code` test uses only single-`$` variables; `$$` in a code fence is NOT asserted.
- Same-line `$$...$$` pairs. The `sameLineBlocks` branch at line 144 has no direct test coverage.

**Proposal:** Three short cases in the `source-renderers: count helpers` block:
- `countMathExpressions("$$\nx=1\n$$\n\n$$\ny=2\n$$")` returns 2.
- `countMathExpressions("\`\`\`\n$$\nnot math\n$$\n\`\`\`")` returns 0.
- `countMathExpressions("Block: $$x=1$$ and $$y=2$$ both.")` returns 2.

## Retry notice liveness ignores session lifecycle and retry sequencing

**Severity:** Medium - `ui/src/SessionPaneView.tsx:900-913` derives connection-retry notice liveness only from whether the message is the latest assistant-authored message.

That is too coarse for the transcript and lifecycle model. If a session leaves the active turn without later assistant output, the retry notice still renders as live with a spinner and `aria-live="polite"`. If one retry notice is followed by another retry notice, the older attempt renders as "Connection recovered" while the newer attempt still renders as "Reconnecting", which presents contradictory connection state.

**Current behavior:**
- The latest assistant-authored message is treated as the only live retry notice.
- Session status is not considered when deciding whether a retry notice is still live.
- Later retry notices are treated the same as later non-retry assistant output, so older retry attempts look resolved while the retry is still in progress.

**Proposal:**
- Derive retry display state from both session lifecycle and subsequent assistant message type.
- Keep the latest retry notice live only while the owning session is active or otherwise busy.
- Treat older retry attempts as superseded while a later retry notice is still the newest assistant output, and mark retry notices resolved only after later non-retry assistant output exists.

## Mermaid demo contains stray placeholder paragraphs

**Severity:** Low - `docs/mermaid-demo.md` now has stray placeholder lines after the Mermaid fenced code block.

This looks like accidental placeholder text and makes the demo document noisier for Markdown/Mermaid validation.

**Current behavior:**
- The Mermaid demo fence is followed by `a`, a blank line, `2`, and `a2`.
- The extra lines are outside the code fence and render as visible Markdown content.

**Proposal:**
- Remove the stray placeholder lines unless they are intentional fixtures.

## Resolved retry notice text diverges from session find indexing

**Severity:** Low - `ui/src/message-cards.tsx:397` renders synthesized resolved-copy for connection retry notices while session find still indexes the stored `message.text`.

Searching for stored retry notice text such as "response finished" or "Retrying automatically" can navigate to the resolved retry card without showing matching highlighted text in the card. That makes find look broken even though the match target is technically correct.

**Current behavior:**
- Session find indexes the persisted retry notice text.
- The resolved retry card replaces that detail with synthesized past-tense copy.
- Search can land on a resolved retry notice that does not visibly contain the searched words.

**Proposal:**
- Keep rendered and searchable text aligned by including the original retry detail in the resolved card.
- Or update the session-find index to use the same derived display text and add coverage for resolved retry notices.

## Settings tab bar missing WAI-ARIA tablist keyboard pattern

**Severity:** Low - `ui/src/preferences/SettingsTabBar.tsx` renders `role="tablist"` + `role="tab"` but does not implement the WAI-ARIA tab keyboard pattern. Arrow keys, Home, and End do nothing, and every tab is individually reachable via Tab (no roving `tabIndex`), so keyboard-first users cycle through every tab instead of jumping laterally within the tablist.

This is a pre-existing gap — the inline tab-bar JSX the component replaced had the same shape — not a regression introduced by the split. The split just makes it a good moment to address it because the tab bar now lives in one small focused file.

**Current behavior:**
- Each `<button role="tab">` has default `tabIndex=0`, so `Tab` traversal visits all seven tabs before leaving the tablist.
- `ArrowLeft` / `ArrowRight` / `Home` / `End` inside the tablist do nothing — the focused tab has no handler.
- Only click (or `Enter` / `Space` via the native button default) selects a tab.

**Proposal:**
- Set `tabIndex={isSelected ? 0 : -1}` on each tab button so `Tab` only reaches the active tab.
- Add an `onKeyDown` on the tablist `<div>` that handles `ArrowLeft` / `ArrowRight` (wrap at ends), `Home` (first tab), and `End` (last tab). On arrow/home/end, call `onSelectTab(next)` and also move DOM focus to the corresponding `<button>` (the caller should re-render with `tabIndex={0}` on the newly-selected tab, which will take focus).
- Keep `Enter` / `Space` / click unchanged — they already work via native button semantics.
- Add a Vitest case that renders the component, focuses the active tab, sends ArrowRight, and asserts the next tab is both selected and focused.

## Settings dialog shell imports its close icon from message rendering

**Severity:** Low - `ui/src/preferences/SettingsDialogShell.tsx:29` imports `DialogCloseIcon` from `../message-cards`, tying the preferences dialog shell to the Markdown/agent message-card module.

This is not a runtime behavior bug, but it weakens the new preferences extraction boundary. Future message-card refactors or lazy-loading work would still have to preserve a general-purpose dialog icon export from the message-rendering module.

`ui/src/message-card-icons.tsx` now exists and owns all six message-card SVG icons including `DialogCloseIcon`; `message-cards.tsx` re-exports it only for backwards compatibility. The clean fix is now a one-line import rewrite.

**Current behavior:**
- `SettingsDialogShell` reaches into `message-cards` for a generic close icon.
- The preferences UI now depends on a module whose primary ownership is message rendering.
- `message-card-icons.tsx` already exists and is the correct source.

**Proposal:**
- Change the `SettingsDialogShell.tsx:29` import to `import { DialogCloseIcon } from "../message-card-icons";` and drop the re-export line from `message-cards.tsx`.
- Import that shared icon from both `message-cards` and `SettingsDialogShell`.
- Keep message-card ownership focused on message rendering rather than generic preferences chrome.

## Settings dialog backdrop dismisses on any mouse button

**Severity:** Low - `ui/src/preferences/SettingsDialogShell.tsx:41` attaches the close handler to `onMouseDown` without guarding the button code. Middle-click (auto-scroll on Windows/Linux) and right-click (context menu on any platform) both fire `mousedown` with `event.button !== 0`, so landing the context menu on the backdrop instead of the dialog body closes the dialog before the menu can open. Same applies to middle-click paste on Linux.

This is a pre-existing shape — the inline JSX the shell replaced had the same handler — not a regression introduced by the split. The split just makes it a clean one-line fix in a focused file.

**Current behavior:**
- `onMouseDown={() => { onClose(); }}` on the backdrop fires for all three primary buttons.
- Right-click on the backdrop closes the dialog and the context menu never appears.
- Middle-click on the backdrop closes the dialog with no scroll affordance.

**Proposal:**
- Switch to `onClick` (which only fires on primary-button click/tap) or guard with `event.button === 0` inside `onMouseDown`.
- Keep the `onMouseDown` + `stopPropagation` on the inner `<section>` so inside-the-card interactions are still protected from the outer handler.
- Add a Vitest case: render the shell, fire `mouseDown({ button: 2 })` on the backdrop, assert `onClose` is not called.

## Mermaid iframe scrollbar reserve lacks focused regression coverage

**Severity:** Low - `ui/src/message-cards.tsx` changed Mermaid iframe height slack from `viewBox height + 8` to `viewBox height + 24`, but no focused test asserts the new normal-size height reserve.

Existing coverage checks pathological max-height clamping, so a regression back to `+ 8` would still pass. The scrollbar-reserve behavior should be pinned with a normal deterministic SVG.

**Current behavior:**
- Production code uses `Math.ceil(dimensions.height) + 24`.
- `MarkdownContent.test.tsx` only asserts that a huge `viewBox` remains capped at the upper bound.

**Proposal:**
- Add a normal-size Mermaid SVG/viewBox test, for example an `80px` viewBox height rendering to a `104px` iframe height.
- Keep the existing huge-viewBox clamp test for the upper-bound security/layout guard.

## Session-find active-hit scroll can fight a user who scrolls away

**Severity:** Low - `ui/src/panels/AgentSessionPanel.tsx:1724-1761` adds a `useLayoutEffect` that writes `node.scrollTop = activeConversationSearchScrollTop` whenever `activeConversationSearchMessageIndex` or `activeConversationSearchScrollTop` changes. Its deps do not include the user's current `node.scrollTop`, so after the initial scroll-to-hit a later measurement commit that changes `layout.tops[activeIdx]` or `messageHeights[activeIdx]` re-runs the effect and snaps the viewport back to the hit — even if the user has since manually scrolled to inspect context around the match.

In practice measurement convergence is fast for visible items, but with measurement churn after a user-initiated scroll-away the hit can snap back for one or two commits. The same effect also unconditionally clears `shouldKeepBottomAfterLayoutRef.current = false`, so once search closes the ref stays `false` and the user has to nudge the viewport before `syncViewport` / `isScrollContainerNearBottom` self-heals sticky-bottom.

**Current behavior:**
- The scroll-to-hit write runs whenever `activeConversationSearchScrollTop` recomputes (layout churn, message-height churn, active-item id change).
- There is no "is the user still near the target" guard before writing `scrollTop`.
- `shouldKeepBottomAfterLayoutRef.current = false` fires on every invocation regardless of whether the effect actually overrides the pin.

**Proposal:**
- Track the last written scroll-top in a ref and only write again if `node.scrollTop` is within a small delta of it (the user hasn't scrolled away).
- Or: only run the pin when `activeConversationSearchMessageId` changes — keep a ref of the last handled active id and compare before writing.
- Move the `shouldKeepBottomAfterLayoutRef.current = false` assignment inside the `Math.abs(...) >= 1` guard so it only fires when the scroll write actually happens.
- Add a Vitest case: open session find, scroll the viewport away from the active hit, trigger a measurement-height change, and assert `scrollTop` is unchanged.

## Stale send responses skip the active-prompt recovery poll

**Severity:** High - a successful `sendMessage` whose returned `StateResponse` is rejected as stale clears the draft and returns before arming the active-prompt safety-net poll.

This happens when SSE has already delivered the user's prompt at a newer same-instance revision before the POST response resolves. The prompt is visible, but if the SSE stream stalls before the first assistant delta, the newly-added early return skips the fallback poll that is supposed to recover missing assistant output.

**Current behavior:**
- `handleSend` calls `adoptState(state)` after `sendMessage` resolves.
- If adoption returns `false`, the branch releases attachments, clears the request error, and returns.
- The active-prompt `/api/state` poll is only scheduled after the adopted branch.

**Proposal:**
- Treat "POST accepted but response snapshot stale" as a successful send for poll scheduling.
- Factor the active-prompt poll setup into a helper and call it after every successful `sendMessage` response while the session is still active.
- Keep the draft-clearing behavior so stale successful responses do not reinsert already-sent text.

## Restart detection accepts late responses from old server instances

**Severity:** Medium - `shouldAdoptSnapshotRevision` treats any non-empty `serverInstanceId` mismatch as a fresh restart, even when the incoming id belongs to a previously-seen old instance.

The intended restart path is "client had instance A, server restarts to unseen instance B, lower revision from B should be accepted." A late response from A after the client already adopted B also differs from the current id, so the helper accepts it before applying revision ordering and can roll the UI back to old-process state.

**Current behavior:**
- The client stores only `lastSeenServerInstanceId`.
- `isServerInstanceMismatch(lastSeen, next)` returns true for any two non-empty different ids.
- The mismatch branch returns true before checking `nextRevision`.

**Proposal:**
- Track a set of seen server instance ids in `App`.
- Treat a different non-empty id as a restart only if it has not been seen before.
- Reject known older ids that differ from the current id, or route them through the normal monotonic revision gate.

## Persist-failure tombstone recovery waits for unrelated mutations to retry

**Severity:** Medium - the persist worker restores drained `removed_session_ids` after `persist_delta_via_cache` fails, but it does not schedule another persist attempt.

If no later state mutation sends another `PersistRequest::Delta`, the restored tombstones remain only in memory. A shutdown before the next unrelated mutation can still leave orphan rows in SQLite, which is the failure mode the tombstone restore was intended to prevent.

**Current behavior:**
- On write error, the worker extends `inner.removed_session_ids` with the drained tombstones.
- The worker logs the error and returns to `persist_rx.recv()`.
- No retry signal or backoff loop is armed for the restored delta.

**Proposal:**
- Re-arm persistence on failure, preferably with a bounded/backoff retry path inside the persist worker.
- Keep the watermark unchanged and recollect after restoring tombstones so changed sessions and deletes retry together.

## Flagship deadline-guard test doesn't isolate the post-await check

**Severity:** Medium - `ui/src/active-prompt-poll.test.ts::stops re-arming after the hard-cap deadline even when fetchState is in flight` (line ~82) claims to pin the post-await `now() >= deadlineMs` guard at `ui/src/active-prompt-poll.ts:125`, but actually exercises a different code path.

Under real fake-timer advance, the belt-and-suspenders hard-cap setTimeout fires first and sets `stopped = true`. When the deferred fetch resolves, the bail-out fires via `if (stopped || !handlers.isMounted()) return;` at line 118 — well before the deadline line. The test would still pass with the post-await deadline check deleted entirely. I noticed this during the "disable fix and verify test fails" verification and even mentioned it in the commit message, but the test docstring still claims to cover line 125 — that's false confidence for future readers and reviewers.

Only the `uses an injectable now()` test at line 394 actually isolates line 125 (its `now()` advance stays independent of the fake timer clock, so the hard-cap setTimeout never fires during the advance window).

**Current behavior:**
- Main deadline test passes due to the `stopped`-flag belt-and-suspenders, not the post-await deadline check.
- Docstring + test name both advertise coverage that only the sibling injectable-now test delivers.
- Regression that drops only `if (now() >= deadlineMs) return;` at line 125 would still pass the main test and only fail the injectable-now test.

**Proposal:**
- Rename the main test to `hard-cap \`stopped\` flag bails an in-flight await when setTimeout cap fires` (matches what it really proves).
- Move the "post-await deadline check" claim from the main docstring to the injectable-now test's docstring (it is the actual regression gate).
- Optionally: add a third test that uses a large `maxDurationMs` in real time + injected `now()` that advances past the deadline, so the hard-cap setTimeout never fires during the advance and the post-await deadline line is the only defense. Would cover both belt-and-suspenders and deadline-check paths independently.

## Silent CRLF→LF conversion on rendered-Markdown save

**Severity:** Medium - the CRLF save fix (`ui/src/panels/markdown-diff-segments.ts` + `ui/src/panels/DiffPanel.tsx`) normalizes `sourceContent` to LF before the resolver runs, then `setEditValueState(nextDocumentContent)` persists that LF-normalized content back into the edit buffer. On a CRLF-on-disk file (common on Windows with `core.autocrlf=true`), the first rendered-Markdown commit silently converts the entire buffer to LF, which gets written to disk on the next save.

Works fine under git `core.autocrlf=true` (git re-applies CRLF on commit), but breaks for users with explicit CRLF-preserving workflows or for files where git is not involved. The `a68091f` commit message acknowledges the trade-off but the UI gives no indication.

**Current behavior:**
- `handleRenderedMarkdownSectionCommits` reads `sourceContent`, LF-normalizes it, applies the commit, writes the LF result via `setEditValueState(nextDocumentContent)`.
- `handleSave` later writes that LF buffer to disk via `onSaveFile`.
- Monaco's source-mode edit path preserves the original line endings; only the rendered-Markdown path does the silent conversion.
- EOL pill at `DiffPanel.tsx:4085` flips from CRLF to LF silently on first rendered edit.

**Proposal:**
- Detect the original EOL style at the sourceContent boundary (CRLF vs LF vs mixed). Keep segment offsets + resolver operations on LF internally, but on save re-apply the original convention via `nextDocumentContent.replace(/\n/g, "\r\n")`.
- Or: add a diff-preview notice when rendered-Markdown mode is active on a CRLF file, making the conversion visible and opt-in.
- Add a Vitest case that loads CRLF content, edits a rendered section, saves, and asserts the saved content still has CRLF line endings.

## `saveError` visibility over-gated by informational `externalFileNotice`

**Severity:** Low - the UX fix that landed in `a68091f` renders `saveError` as a diagnostic note, gated by `!externalFileNotice && !diffEditConflictOnDisk`. But `externalFileNotice` is sometimes set to an informational (non-error) string such as `"Rendered Markdown edits will save this document to the worktree file."` (`DiffPanel.tsx:1185`, `1227`). If a save fails while such an informational notice is visible, the user sees the "Save failed" pill with no diagnostic — the exact regression the UX fix was landed to prevent.

**Current behavior:**
- `saveError` diagnostic note rendered only when `externalFileNotice` AND `diffEditConflictOnDisk` are both falsy.
- Informational `externalFileNotice` values suppress the diagnostic even though they carry no error semantics.

**Proposal:**
- Gate only on `diffEditConflictOnDisk` (that branch renders its own recovery UI with "Apply my edits" / "Save anyway" / "Reload from disk" buttons, where the conflict message is obvious from the button labels).
- Or: render `saveError` unconditionally alongside `externalFileNotice` with a visual distinction (e.g., red border for errors, gray for notices).
- Add a Vitest case that sets an informational `externalFileNotice`, triggers a save failure, and asserts the saveError text is visible.

## Duplicated `if changed { publish_delta }` block across remote proxy files

**Severity:** Low - `src/remote_create_proxies.rs` and `src/remote_codex_proxies.rs` now have near-identical 40-line blocks with the same `(revision, local_session_id, local_session, changed)` tuple destructure + gated `publish_delta(DeltaEvent::SessionCreated { ... })`. The rationale comment is replicated in both files with "See the identical comment in `remote_create_proxies.rs`". Two sites drifting apart (e.g., one adding a new DeltaEvent variant the other misses) is the kind of subtle inconsistency that's hard to notice during review.

**Current behavior:**
- Both files duplicate the gated-publish pattern inline.
- Cross-reference comment is the only guard against drift.
- No shared helper encapsulating the invariant "announce only when the local record actually changed".

**Proposal:**
- Extract `AppState::announce_session_created_if_changed(&self, changed: bool, revision: u64, local_session: &Session)` (or a similar signature) that encapsulates the `if changed { publish_delta(&DeltaEvent::SessionCreated { ... }) }` block.
- Both proxy paths call it. Single source of truth, compile-visible enforcement of the invariant.

## `startActivePromptPoll` leaves hard-cap timer pending after natural onState stop

**Severity:** Low - when the chained poll stops naturally because `onState` returns `true` (session becomes idle), the chained timer is not re-armed but the belt-and-suspenders `hardCapTimerId` setTimeout stays pending for up to 5 minutes until its callback fires harmlessly. Minor timer churn on fast-turn workflows; not a memory leak since the callback eventually clears itself.

**Current behavior:**
- `onState` returning `true` hits the `if (shouldStop) return;` branch in the chained-setTimeout async callback.
- That branch does not clear `hardCapTimerId`; the timer stays armed until the cap elapses.
- Many fast-turn prompt cycles accumulate dangling 5-minute timers in the JS runtime (each firing an empty cancel).

**Proposal:**
- When `onState` returns `true`, clear `hardCapTimerId` as well — either call the internal cancel body inline, or extract a `stopAndClear()` helper shared by the natural-stop path and the returned `cancel` function.
- Add a Vitest case that exercises the natural-stop path and asserts `vi.getTimerCount()` drops to zero after `onState` returns `true`.

## `shared_codex_thread_setup_persist_failure_does_not_tear_down_runtime` test flake

**Severity:** Low - `tests::shared_codex::shared_codex_thread_setup_persist_failure_does_not_tear_down_runtime` was observed failing intermittently during batched `cargo test --bin termal` runs. Passes when re-run in isolation. The two Gemini-auth siblings (`select_acp_auth_method_ignores_workspace_dotenv_credentials` and `gemini_dotenv_env_pairs_ignore_workspace_env_files`) were fixed by acquiring `TEST_HOME_ENV_MUTEX` and isolating HOME + Gemini/Google env vars; verified via 5 consecutive green `cargo test --bin termal` runs. The shared-codex test did not surface in those 5 runs, so either (a) it is much rarer than the Gemini one, (b) it was indirectly fixed by an unrelated change, or (c) it is still broken but the window is too narrow to hit.

**Current behavior:**
- Pass-in-isolation, fail-in-batch pattern when it surfaces.
- Unlike the Gemini flakes, this test does not obviously share HOME-rooted fixtures — likely a temp-file path collision or a side effect of persist-thread teardown.
- Has not surfaced in recent multi-run verification, so concrete reproduction is not yet captured.

**Proposal:**
- Reproduce via a regression harness that runs the test 20 times back-to-back under the full batch context; confirm the flake signature (temp-file collision vs env var vs persist-thread handle leak).
- If the flake is temp-file path collision: switch to `tempfile::tempdir()` with unique per-test directories.
- If env: add `TEST_HOME_ENV_MUTEX` acquisition and `ScopedEnvVar::remove` isolation to match the Gemini pattern.
- Document the root cause in the fix commit message so the "why mutex / why tempdir" is visible at review time.

## Stale `src/tests.rs` can reappear in the working tree alongside `src/tests/mod.rs`

**Severity:** Low - during the April 16 session split of `src/tests.rs` into the `src/tests/` directory, a copy of the original pre-split file was observed reappearing in the working tree as a staged "new file" after later commits. The cause is not fully understood (possibly an IDE cache, a tool operation that restored an earlier snapshot, or accidental `git add` glob behavior). Rustc errors out with `E0761: file for module 'tests' found at both "src\tests.rs" and "src\tests\mod.rs"` under `cargo check --tests`, though `cargo test --bin termal` can sometimes still resolve the module to `src/tests/mod.rs` and pass, masking the problem.

**Current behavior:**
- The stale file reappeared at least once after the directory form was already committed; it was staged as `new file: src/tests.rs`.
- `cargo check --tests` fails immediately; `cargo test --bin termal` may or may not, depending on invocation path.

**Proposal:**
- Add a `.gitignore` or `.gitattributes` guard against a literal `src/tests.rs` path while the directory form is in use.
- Prefer running `cargo check --tests` (not just `cargo test`) in local verification, since the bin-only invocation can miss the ambiguity.
- If the reappearance was an IDE-side artifact, consider documenting the offending tool so future contributors avoid the same mistake.

## `adoptCreatedSessionResponse` recovery still opens a fallback workspace pane

**Severity:** Low - the new `created.session.id !== created.sessionId` branch requests a recovery resync, but it still returns `false`. All current call sites interpret `false` as "adoption failed, open `created.sessionId` in the workspace anyway."

For a malformed create/fork response, the session is not inserted into `sessionsRef`, but the workspace fallback can still create a pane for `created.sessionId`. The resync may later remove or repair it, but until then the UI can show a phantom or blank session pane.

**Current behavior:**
- Mismatch branch calls `requestActionRecoveryResyncRef.current()` and returns `false`.
- Create/fork call sites open `created.sessionId` on `!adopted`.
- There is no typed distinction between "not adopted yet" and "recovery in progress; do not trust this id."

**Proposal:**
- Return a discriminated result such as `adopted | stale | recovering` from `adoptCreatedSessionResponse`.
- Suppress workspace fallback opening for protocol mismatch/recovery.
- Add Vitest coverage that a mismatched create response triggers recovery without inserting or opening the mismatched session.

## Remote sync rollback test does not compare full rollback contents

**Severity:** Low (test robustness) - `failed_remote_snapshot_sync_restores_session_tombstones` now checks that rollback restores session IDs, tombstones, session numbering, and orchestrator count, but it does not compare full session records or full orchestrator instance contents.

A future regression could restore the right IDs while leaving mutated session fields, remote metadata, project IDs, or orchestrator payloads behind. The test would still pass even though `RemoteSyncRollback` is intended to restore the complete captured state for the fields it owns.

**Current behavior:**
- Test asserts session-id membership/count and orchestrator count.
- It does not assert full `SessionRecord` or `OrchestratorInstance` equality/content.

**Proposal:**
- Compare the full captured session records and orchestrator instances after rollback, or at least compare full `Session` values plus remote metadata and complete orchestrator instance contents.

## Server restart without browser refresh can lose the last streamed message

**Severity:** Medium - restarting the backend process while the browser tab is still open can make the most recent assistant message disappear from the UI, because the persist thread has a small window between "commit fires" and "row is durably in SQLite" during which an un-drained mutation is lost on kill.

Persistence is intentionally background and best-effort: every `commit_persisted_delta_locked` (and similar delta-producing commit helpers) signals `PersistRequest::Delta` to the persist thread and returns. The thread then locks `inner`, builds the delta, and writes. If the backend process is killed (SIGKILL, laptop sleep wedge, crash, manual restart of the dev process) between the signal fire and the SQLite commit, the mutation is lost. Old pre-delta-persistence behavior had the same window — the persist channel carried a full-state clone — so this is not a regression introduced by the delta refactor, but the symptom is visible now because the reconnect adoption path applies the persisted state with `allowRevisionDowngrade: true`: the browser's in-memory copy of the just-streamed last message is replaced by the freshly loaded (older) backend state, making the message disappear from the UI.

The message is not hidden; it is genuinely gone from SQLite. No amount of frontend re-rendering will bring it back.

**Current behavior:**
- Active-turn deltas (e.g., streaming assistant text, `MessageCreated` at the end of a turn) commit through `commit_persisted_delta_locked`, which only signals the persist thread.
- The persist thread acquires `inner` briefly, collects the delta, and writes to SQLite.
- Between "signal sent" and "row written" there is a small time window (usually sub-millisecond, but can stretch under contention) during which a hard kill of the backend loses the mutation.
- On backend restart + SSE reconnect, the browser's `allowRevisionDowngrade: true` adoption path applies the persisted state. The persisted state is missing the un-drained mutation, so the in-memory latest message is overwritten and disappears.

**Proposal:**
- **Graceful-shutdown flush**: install a `SIGTERM` / `Ctrl+C` handler that drains the persist channel before the process exits, so user-initiated restarts (the common case) never lose data.
- **Opt-in synchronous persistence** for the last message of a turn: the turn-completion commit (`finish_turn_ok_if_runtime_matches`'s `commit_locked`) could flush synchronously before returning, trading a few ms of latency on turn completion for zero-loss durability of the final message.
- **Accept and document** as a known limitation that hard process kills (SIGKILL, power loss) can lose at most the last un-drained commit. Add a line to `docs/architecture.md` describing the background-persist durability contract.
- A regression test that exercises "restart backend mid-turn, reconnect browser, assert the final message is visible" would pin whichever fix is chosen; without the fix it is expected to fail.

## `SqlitePersistConnectionCache` has no error-driven invalidation

**Severity:** Medium - once the cached SQLite connection enters a persistent error state, every subsequent persist tick silently logs the same error. No auto-recovery.

`SqlitePersistConnectionCache::connection_for(path)` at `src/api.rs:353-397` only swaps the connection when `path` changes. On a persistent SQLite error (`SQLITE_READONLY`, `SQLITE_CORRUPT`, `SQLITE_FULL`, the backing file unlinked by a user "reset", a Windows-side handle issue after a crash), every subsequent call reuses the broken handle. Errors land in the persist-thread log, but the cache never reopens, so the process can get stuck in a permanent "persist broken" state that a backend restart would repair.

**Current behavior:**
- `persist_delta_via_cache` grabs the cached connection, builds a transaction, commits.
- On transaction or commit failure, the error propagates up and the persist thread logs it.
- The cache still holds the same broken connection. Next tick: same error, same log, forever.

**Proposal:**
- On persist error, drop the cached connection (`cache.connection = None; cache.path = None;`) so the next tick reopens and re-runs `ensure_sqlite_state_schema`.
- Accept the cost of the reopen on error; the happy path still reuses one connection per process lifetime.
- Add a regression test: seed an error that the cache should recover from (e.g., unlink the backing file after a successful write) and assert the next persist tick creates a new connection and writes successfully.

## Unix terminal shell spawn dropped the login-shell flag

**Severity:** Medium - the `/api/terminal` Unix spawn path changed from `sh -lc` to `sh -c`, so the terminal no longer sources `.profile`/`.bash_profile`. Users who rely on those for `PATH` additions (`nvm`, `uv`, `poetry`, homebrew prefixes) will find their tools missing from the terminal panel.

At `src/api.rs:2797`, the diff dropped the `-l` flag. `sh -c` runs in non-login mode, which skips profile sourcing on most user configurations.

**Current behavior:**
- Terminal panel spawns `sh -c <command>` on Unix instead of `sh -lc <command>`.
- Commands run without the user's login-shell `PATH` adjustments.
- A user whose `node` / `uv` / `poetry` / `gcloud` is only on PATH via `.profile` gets "command not found" in the terminal panel.

**Proposal:**
- Restore `sh -lc` unless there is a documented reason (e.g., the login-shell init was measurably slow and intentionally removed).
- If the removal was intentional, add a comment explaining why and document the expectation that `PATH` must be set at the parent-process level.

## Mermaid dimension cap missing negative/zero test coverage

**Severity:** Medium - `clampMermaidDiagramExtent` regex accepts `[-+]?` signed values, and `readMermaidSvgDimensions` only rejects non-finite numbers. The existing "huge viewBox" test covers the upper clamp; nothing covers the lower clamp.

A hostile or buggy agent output can produce `viewBox="0 0 -50 -50"` or `viewBox="0 0 0 0"`. The current test in `ui/src/MarkdownContent.test.tsx:320-347` asserts only that a 10,000×10,000 viewBox is clamped to the upper bound. A regression that drops `Math.max(lowerBound, …)` from `clampMermaidDiagramExtent` would pass the current tests.

**Current behavior:**
- Upper bound is tested (10,000 → 4096).
- Lower bound is untested. Negative or zero input behavior depends on `Math.min(Math.max(lowerBound, value), upperBound)` still being intact in production code.

**Proposal:**
- Add two tests: `viewBox="0 0 -100 -100"` (negative → clamp to lower bound) and `viewBox="0 0 0 0"` (zero → clamp to lower bound). Assert the rendered widthPx/heightPx stay in `[lowerBound, upperBound]`.

## `MessageCard` default-prop inline arrows defeat memoization

**Severity:** Low - two optional callback props default to fresh inline arrow functions, so the new strict `===` memo comparator will always report them as different when the parent omits them, forcing a re-render on every parent render.

`MessageCard` destructures `onMcpElicitationSubmit = () => {}` and `onCodexAppRequestSubmit = () => {}` at `ui/src/message-cards.tsx:105-117`. Each parent render allocates a new default function. The comparator added at lines 327-328 compares these with `===` and always fails on the optional-and-omitted case.

**Current behavior:**
- Parent renders a `MessageCard` without the two optional callbacks.
- Each render, a fresh default arrow is passed.
- Comparator sees a "changed" prop and re-renders, even when nothing the user sees has changed.

**Proposal:**
- Hoist the two no-op defaults to module scope for stable identity, or drop them from the comparator when the parent can guarantee they are always passed.

## React dep-array hygiene: stale-or-extraneous deps across three hot effects

**Severity:** Low - three `useEffect` hooks over-list deps that either re-trigger the effect for no behavioral reason (wasting work) or cause observer churn. Minor perf only; no correctness impact.

- `ui/src/App.tsx:2203-2207` — hydration effect lists `activeSession?.messages.length` in deps; body only reads `activeSession?.id` and `activeSession?.messagesLoaded`. Every streamed token reruns the effect and early-returns.
- `ui/src/message-cards.tsx:2854` — Markdown line-marker `useEffect` was extended to include `documentPath`, `hasOpenSourceLink`, `workspaceRoot` in deps. The body reads none of them; only `showLineNumbers` and the `markdownRootRef` DOM. Triggers `ResizeObserver` tear-down + rebuild on unrelated context changes.
- `ui/src/App.tsx:4734-4737` — the 5-minute hard-cap `setTimeout` is not cleared when the poll chain exits because the session left "active" status. The handler no-ops, so it is not a leak — just a pending timer slot held for up to 5 minutes per completed prompt.

**Proposal:**
- Drop `activeSession?.messages.length` from the hydration effect deps.
- Drop `documentPath`, `hasOpenSourceLink`, `workspaceRoot` from the line-marker effect deps.
- In the early-return branch of the safety-net poll, clear `activePromptPollTimeoutRef.current` alongside the chain ref.

## Read-only Markdown input flashes plain source for a frame

**Severity:** Low - when the user tries to edit a read-only Markdown segment, the handler imperatively sets `event.currentTarget.textContent = segment.markdown` before triggering the `onReadOnlyMutation` remount. For one paint frame the rendered Markdown subtree is replaced by raw source text.

At `ui/src/panels/DiffPanel.tsx:2785-2790`, the read-only branch of `handleInput` assigns raw text to `textContent` and then bumps `readOnlyResetVersion` to remount. The textContent assignment is unnecessary — `onReadOnlyMutation()` alone triggers the remount and React will reconcile the correct rendered DOM on the next commit.

**Current behavior:**
- User attempts a disallowed edit.
- Plain source text flashes for one paint frame.
- Remount completes, rendered Markdown returns.

**Proposal:**
- Drop the `event.currentTarget.textContent = segment.markdown` assignment; rely on `onReadOnlyMutation()` to trigger the remount.

## `session_mut` helpers stamp eagerly before the caller decides to mutate

**Severity:** Low - check-then-early-return paths advance the mutation stamp even when no field actually changed, so the persist thread re-serializes the session on the next tick for no reason. Softly undoes the delta-persist benefit.

`session_mut_by_index` and `session_mut` both bump `last_mutation_stamp` and write it to the record before returning `&mut SessionRecord`. Several callers acquire the mut borrow, read a field, decide nothing needs to change, and return. `sync_session_cursor_mode`, `set_agent_commands`, and several `clear_stopped_orchestrator_queued_prompts` sites follow this pattern. The stamp is permanent, so `collect_persist_delta` on the next commit sees the session as dirty and writes its row.

**Current behavior:**
- `session_mut*` stamps on access, before the caller decides.
- Check-then-early-return callers spuriously mark sessions dirty.
- Persist thread writes unchanged session rows on follow-up commits.
- Cost is small per-instance but compounds across many mutation sites.

**Proposal:**
- Add a read-only `session_by_index(index) -> Option<&SessionRecord>` helper for read-first callers.
- Callers that need to mutate after the read switch to `stamp_session_at_index(index)` explicitly before mutating, or re-borrow through `session_mut_by_index` only when certain.
- Alternatively: change `session_mut*` to return a guard type that stamps on drop only if the caller called a `mark_mutated()` method — more invasive.

## SSE state broadcaster can reorder state events against deltas

**Severity:** Medium - under a burst of mutations, a delta event can arrive at the client before the state event for the same revision, triggering avoidable `/api/state` resync fetches.

Before the broadcaster thread, `commit_locked` published state synchronously (`state_events.send(payload)` under the state mutex), so state N always hit the SSE stream before any follow-up delta N+1. Now `publish_snapshot` enqueues the owned `StateResponse` to an mpsc channel and returns; the broadcaster thread drains and serializes on its own schedule. `publish_delta` remains synchronous. A caller that does `commit_locked(...)?` + `publish_delta(...)` can therefore race: the delta hits `delta_events` before the broadcaster drains state N. The frontend's `decideDeltaRevisionAction` requires `delta.revision === current + 1`; if state N hasn't advanced `latestStateRevisionRef` yet, the delta is treated as a gap and the client fires a full `fetchState`.

**Current behavior:**
- `publish_snapshot` is async (channel + broadcaster thread).
- `publish_delta` is sync.
- Client can observe delta N+1 before state N.
- Extra `/api/state` resync fetches fire under sustained mutation bursts.
- Correctness preserved (resync fixes the view), but behavior is chatty and pushes load onto `/api/state` — which is exactly the path we just made cheaper.

**Proposal:**
- Route deltas through the same broadcaster thread so state and delta events for the same revision stream in order. Coalescing is fine because deltas are idempotent after a state snapshot.
- Or: have `publish_snapshot` synchronously send a revision-only "marker" into `state_events` immediately and let the broadcaster thread serialize and send the full payload; the client's `latestStateRevisionRef` advances on the marker.
- Or: document the tradeoff and rely on the existing `/api/state` resync fallback; track the extra traffic.

## SSE state broadcaster queue can grow before coalescing

**Severity:** Low - bursty commits can enqueue multiple full `StateResponse` snapshots before the broadcaster gets a chance to drop superseded ones.

The broadcaster thread coalesces snapshots only after receiving from its unbounded `mpsc::channel`. During a burst of commits, the sender side can enqueue several large snapshots first, so the "newest only" behavior does not actually bound queued memory or provide backpressure.

**Current behavior:**
- `publish_snapshot` sends owned `StateResponse` values to an unbounded channel.
- The broadcaster drains and coalesces only after snapshots have already queued.
- Full-state snapshots can accumulate during bursts even though older snapshots will be superseded.

**Proposal:**
- Replace the unbounded queue with a single-slot latest mailbox or bounded channel.
- Drop or overwrite superseded snapshots before they can accumulate in memory.
- Add a burst test that publishes multiple large snapshots while the broadcaster is delayed and asserts only the latest snapshot is retained.

## `persist_state_from_persisted_with_connection` clones the full state then clears sessions

**Severity:** Low - the test-fallback and any synchronous-persist call site deep-clones every session transcript, then discards the clones to produce metadata.

`let mut metadata = persisted.clone(); metadata.sessions.clear();` — every transcript is deep-copied just to drop it. Pre-existing pattern; the delta work didn't introduce it but also didn't fix it.

**Current behavior:**
- Every synchronous persist call allocates MBs only to discard them.
- The same pattern lives in `persist_persisted_state_to_sqlite`.

**Proposal:**
- Take `PersistedState` by value where possible and `std::mem::take(&mut persisted.sessions)` into a local; reuse the remaining `persisted` as metadata.
- Or: add a dedicated `PersistedState::split_into_metadata_and_sessions(self) -> (Self, Vec<PersistedSessionRecord>)`.

## Mermaid iframe `max-width: 100%` can be defeated by a flex ancestor

**Severity:** Low - the dimension cap correctly bounds the iframe's intrinsic width at 4096 px, but `max-width: 100%` only binds if no ancestor sizes the child by intrinsic content.

Common React-flex pitfall: a flex child with an intrinsic width of 4096 px forces the parent to that width even with `max-width: 100%`, because flex items default to `min-width: auto` which prevents shrinking below content size. The cap helps layout not break at 10 000+ px, but does not guarantee the iframe scales with the viewport on every ancestor layout.

**Current behavior:**
- `.mermaid-diagram-frame { max-width: 100% }` is set in CSS.
- Inline `maxWidth: "100%"` is set in the computed style.
- If an ancestor column or flex container does not set `min-width: 0`, a 4096 px iframe still forces its container to 4096 px.

**Proposal:**
- Add `.mermaid-diagram-frame { min-width: 0; }` explicitly so the iframe can shrink below its intrinsic width.
- Or: ensure a known ancestor column sets `min-width: 0` / `overflow-x: auto` for Mermaid blocks.
- Add a regression test with a narrow-column ancestor that asserts the iframe's rendered width does not exceed the column.

## State snapshots still include full session transcripts on the wire

**Severity:** Medium - `/api/state` response bodies and SSE state broadcasts still include every session's full `messages` vector. The serialization CPU cost is now off the mutex and off the tokio workers (broadcaster thread + `spawn_blocking`), but the payload size itself is unchanged.

`snapshot_from_inner_with_agent_readiness` continues to clone every visible `Session` with its full `messages` vector into `StateResponse`. The HTTP `/api/state` handler and SSE state publisher then serialize those full transcripts even when the frontend only needs session metadata. Reconnect and tab-restore payloads still scale with total transcript size; individual active-prompt latency is unblocked (per the delta-persist and broadcaster fixes above), but network/client time to apply a full-state snapshot still scales.

**Current behavior:**
- `/api/state` returns all visible sessions with all historical messages (serialized inside `spawn_blocking`, so no tokio worker stall, but the response body is still O(all messages)).
- `publish_state_locked` builds the same full transcript snapshot for SSE state events (serialized on the broadcaster thread).
- The dedicated `GET /api/sessions/{id}` route exists, but state snapshots do not defer to it.
- The frontend already has `Session.messagesLoaded?: boolean` scaffolding that treats `false` as "needs hydrate" — forward-compat for the planned backend change.

**Proposal:**
- Make state snapshots metadata-first: include session shell fields and mark transcript-bearing sessions as `messagesLoaded: false` with an empty `messages` array.
- Keep `GET /api/sessions/{id}` as the authoritative full-transcript route, and keep session-create/prompt flows returning enough data that the active prompt UI remains reliable.
- **Before landing** (per the earlier revert): audit every `commit_locked` caller and ensure a matching `publish_delta` exists for any state change that adds/edits messages, so stripped state events do not drop the change.
- Add backend and App-level regression coverage proving `/api/state` omits transcripts, session hydration restores the full transcript, and metadata snapshots do not clear an already-hydrated active session.

## `commit_session_created_locked` performs synchronous SQLite I/O under the state mutex

**Severity:** High - session creation now holds the `Arc<Mutex<StateInner>>` across a full SQLite transaction, blocking every concurrent request behind disk I/O.

The new `commit_session_created_locked` path in `src/state.rs` calls `persist_created_session`, which in production opens a SQLite connection, runs `ensure_sqlite_state_schema`, starts a transaction, writes metadata plus the created session row, commits, and closes — all synchronously while the `inner` mutex is held. The existing `persist_internal_locked` pattern explicitly offloads persistence to a background thread via `persist_tx` specifically so other requests are not blocked behind disk I/O (see its doc comment). The new path defeats that invariant and regresses session-create latency under contention (e.g., an SSE publisher trying to read state, or a burst of session creations).

**Current behavior:**
- `commit_session_created_locked` runs `persist_created_session` synchronously.
- `persist_created_session` opens a SQLite connection, runs schema-ensure, transactional metadata + session upsert, commit, close — all under the state mutex.
- Any other request that calls `self.inner.lock()` (including SSE publish paths) blocks behind the disk write.

**Proposal:**
- Route `persist_created_session` through the same `persist_tx` background channel used by `persist_internal_locked`. Add a new `PersistRequest` variant or reuse the existing one with just the changed session payload.
- At minimum, drop the state mutex before calling `persist_created_session` and accept the race window for the in-memory revision-vs-persisted divergence.
- Add a test that measures the state-mutex hold duration across a session create and asserts it stays under a small budget.

## SQLite persistence lacks file permission hardening and indefinite backup retention

**Severity:** Medium - session history including agent output, user prompts, and captured file contents is readable by other local users on default Unix systems, and a second sensitive copy is kept indefinitely at a predictable path.

The new SQLite persistence path opens `~/.termal/termal.sqlite` via `rusqlite::Connection::open` without setting restrictive permissions; on Unix, the default `umask 0022` yields world-readable `0644`. The JSON→SQLite migration renames the legacy file to `sessions.imported-<timestamp>.json` (same permissions) and never deletes or surfaces it, so the full pre-migration history persists at a predictable path with no garbage collection or user notice.

**Current behavior:**
- `rusqlite::Connection::open` creates the DB with the current umask (0644 by default on Unix).
- `imported_json_backup_path` writes to a predictable directory alongside the DB.
- No GC, no UI notification of the backup path, no explicit "delete imported backup" action.

**Proposal:**
- On Unix, call `fs::set_permissions(path, Permissions::from_mode(0o600))` on both the SQLite DB and the imported backup immediately after open/rename.
- On Windows, document the reliance on `%USERPROFILE%\.termal\` ACL inheritance; optionally tighten via `SetNamedSecurityInfo`.
- Either delete the imported backup after a successful cold start confirms the SQLite file is usable, or emit a one-shot UI notice with the backup path and an explicit delete affordance.

## SQLite load path still opens a fresh connection and double-queries app state

**Severity:** Low - the first SQLite slice is restart-safe, but `load_state_from_sqlite` still opens a fresh connection and eagerly evaluates a fallback app-state key on the happy path.

The background persist thread now caches a single SQLite connection for its lifetime (`SqlitePersistConnectionCache`), and `ensure_sqlite_state_schema` runs only on the first open. The load path remains unchanged: each startup still opens a fresh connection (acceptable — it's a one-shot) and uses `.or(...)` where `if let Some(..) else { .. }` would avoid a redundant query on the happy path.

**Current behavior:**
- `load_state_from_sqlite` opens a fresh connection on every startup.
- Eager `.or(...)` in `load_state_from_sqlite` evaluates the legacy app-state lookup even when the primary key is present.

**Proposal:**
- Convert `.or(...)` to a lazy `if let Some(..) else { .. }`.
- Optionally share the cached connection across load/persist if any post-startup load path emerges.

## `persist_created_session` skips hidden Claude spare pool changes

**Severity:** Medium - a crash after session creation but before a full snapshot loses changes to the hidden-spare pool that `create_session` may have triggered.

`persist_created_session` in `#[cfg(not(test))]` writes only the created session's record plus metadata, with `replace_sessions=false`. `create_session` can also invoke `try_start_hidden_claude_spare` to replenish the hidden-spare pool, which adds new session records to `inner.sessions` outside the created-session record. Those new hidden records are not part of the `persist_created_session` call and will not reach SQLite until the next `persist_internal_locked` snapshot runs.

**Current behavior:**
- `persist_state_parts_to_sqlite(..., &[record], replace_sessions=false)` upserts only the created record.
- Hidden Claude spares spawned by `try_start_hidden_claude_spare` live only in memory until a later full commit.
- A crash in the window loses the spare pool; the pool can be respawned on demand so impact is bounded.

**Proposal:**
- Include all sessions whose in-memory state changed during the create (the created record plus any newly spawned hidden spares) in the `persist_created_session` call.
- Or follow the delta-style write with a `persist_internal_locked` snapshot once the spare pool is settled.

## Lazy hydration effect: missing retry guard, over-eager deps, unreconciled replace

**Severity:** Medium - the new hydration path has several bugs that will materialize once the backend starts emitting `messagesLoaded: false` sessions.

Three distinct issues in and around the new `useEffect(... fetchSession ...)` in `ui/src/App.tsx`:
1. The dep array includes `activeSession?.messages.length`, causing the effect to re-run on every SSE `textDelta` token for the active session. Today the body short-circuits via the hydrated-set, so no correctness issue — but the deps are a footgun for any future real work added to the effect.
2. The async IIFE only guards against unmount. If the user switches away mid-fetch and the response's `session.id !== sessionId`, the code calls `requestActionRecoveryResyncRef.current()`. A transient server race can loop mismatch → resync → refetch → mismatch.
3. `adoptCreatedSessionResponse` (and `live-updates.ts`'s `sessionCreated` reducer) raw-replace an existing session without per-message identity preservation via `reconcileSession`. If SSE `sessionCreated` materializes the session before the API response lands (or vice versa), memoized `MessageCard` children see new identities and remount.

**Current behavior:**
- Deps: `activeSession?.id`, `activeSession?.messages.length`, `activeSession?.messagesLoaded`.
- Mismatch branch triggers action-recovery resync without a "tried once" marker.
- Raw `[...previousSessions, created.session]` / `replaceSession(..., delta.session)` on the `existingIndex !== -1` branch.

**Proposal:**
- Drop `activeSession?.messages.length` from the dep array; comment the deliberate exclusion.
- Add a `hydrationMismatchSessionIdsRef` (or count attempts) to avoid re-firing after one mismatch until an authoritative state event arrives.
- Route the existing-session replace branch through `reconcileSession` (or a similar identity-preserving merge) so memoized children keep stable identity.

## `isSafePastedMarkdownHref` Windows drive-letter exception inconsistent with protocol allowlist

**Severity:** Low - pasted `<a href="C:\...">` links are accepted as "safe" even though the function advertises a strict protocol allowlist; inert in a browser today but a latent hazard if hrefs are ever opened via a native handler.

`isSafePastedMarkdownHref` short-circuits on `[a-zA-Z]:[\\/]` and returns `true` before the protocol allowlist check runs. This accepts arbitrary local filesystem paths on Windows. In a browser, clicking such an href is inert (modern browsers refuse `file://` from an http origin), but if TermAl ever ships a Tauri/Electron wrapper or a native link opener, this becomes arbitrary local-file invocation.

**Current behavior:**
- `/^[a-zA-Z]:[\\/]/.test(trimmed)` short-circuits to `true`.
- The `http/https/mailto` protocol allowlist never sees Windows drive-letter paths.
- Pasted `<a href="C:\Windows\System32\cmd.exe">` survives sanitization with its href intact.

**Proposal:**
- Drop the drive-letter short-circuit. If local-path Markdown links are a product need, handle them through a constrained opener, not generic `<a href>`.
- Add a test asserting `<a href="C:\foo">` loses its href through sanitization.

## 404 on `fetchSession` surfaces as a user-visible request error instead of silent resync

**Severity:** Low - a benign race where a session is deleted or hidden between a delta event and the hydration fetch becomes a toast.

The new hydration effect's error path calls `reportRequestError(error)` on any `fetchSession` failure, including 404 from `find_visible_session_index`. A benign deletion race (session hidden while fetch is in flight) becomes a toast plus inline recovery affordance, instead of a silent state resync.

**Current behavior:**
- `fetchSession` 404 → `reportRequestError(error)`.
- User sees an error toast for a race that should be invisible.

**Proposal:**
- Special-case 404 on `fetchSession` to call `requestActionRecoveryResyncRef.current()` without `reportRequestError`, similar to how `fetchWorkspaceLayout` treats 404.

## `handleApplyDiffEditsToDiskVersion` silently continues when rendered Markdown commit batch conflicts

**Severity:** Medium - the apply-to-disk-version button can silently no-op or rebase against stale state when an unmappable rendered Markdown draft is in the batch.

`handleApplyDiffEditsToDiskVersion` now calls `flushSync(() => commitRenderedMarkdownDrafts())` at the top to capture active DOM drafts before rebasing. When the batch contains an unmappable or overlapping section, `handleRenderedMarkdownSectionCommits` sets a `setSaveError(...)` banner and returns `false`, leaving the drafts dirty. The handler does not inspect the commit result: it proceeds to read `editValueRef.current`, which may still be the pre-flush value, and either short-circuits via the `currentEditValue === currentFile.content` path or rebases with stale content. The user clicks a specific button and gets only the commit error banner; the apply-to-disk-version action itself appears to have done nothing.

**Current behavior:**
- `flushSync(() => commitRenderedMarkdownDrafts())` runs at the top of the handler.
- `commitRenderedMarkdownDrafts` → `handleRenderedMarkdownSectionCommits` returns `false` for a conflict but the caller discards the return value.
- The rebase path then proceeds, may silently return via the early shortcut, and the user has no dedicated notice for the apply-to-disk-version action.

**Proposal:**
- Capture the `flushSync`'d commit result and, when drafts were not applied cleanly, short-circuit with an explicit notice (e.g., `setExternalFileNotice("Resolve rendered Markdown conflicts before applying edits to the disk version.")`) before touching `fetchFile` / rebase.
- Add coverage where a rendered Markdown section cannot be mapped and the user clicks apply-to-disk-version, asserting the specific notice is shown and `fetchFile` is not called.

## Implementation Tasks

- [ ] P2: Add focused control-panel-layout.ts coverage:
  add `ui/src/control-panel-layout.test.ts` covering the four currently
  untested exports. Cases worth pinning:
  `getDockedControlPanelWidthRatioForWorkspace` with the control panel on
  left, on right, and with a non-split workspace root;
  `resolvePreferredControlPanelWidthRatio` with a saved ratio above the
  CSS-derived minimum (both first-pane and second-pane placement);
  `hydrateControlPanelLayout` for left vs right side, empty workspace, and
  a workspace that already has a dock; and `resolveRootCssLengthPx` with
  `calc(40 * 1rem)` and `calc(1rem * 40)`, `rem`-to-px conversion, and a
  `var(--fallback-chain)` resolution case.
- [ ] P2: Add focused `SettingsDialogShell` coverage:
  render the shell with children, fire `mouseDown` on the backdrop and
  inside the dialog, and assert only the backdrop path calls `onClose`.
  Keep a close-button assertion. Include a right-click backdrop case when
  the mouse-button guard is fixed. Also snapshot the ARIA wiring
  (`role="dialog"`, `aria-modal="true"`, `aria-labelledby`, id
  `settings-dialog`, `<h2 id="settings-dialog-title">`).
- [ ] P2: Tighten the session-find virtualization test:
  `ui/src/panels/AgentSessionPanel.test.tsx` asserts fewer than 80 cards
  render for 180 messages during search, which is correct but loose. The
  working range is closer to 14-28 cards given viewport=500 / overscan=960
  / default-height=180. Tighten to `< 40` and add an exclusion assertion
  like `expect(screen.queryByText("message-5")).toBeNull()` so a
  regression where the merged range falls back to `{0, messages.length}`
  fails loudly.
- [ ] P2: Add `SessionPaneView` retry-notice state coverage:
  exercise the real session rendering path, not just direct
  `MessageCard` props. Use rerendered session messages for retry notice
  -> user message -> later retry notice -> later non-retry assistant output,
  and assert the notice remains live only when appropriate, superseded retry
  attempts do not claim recovery, resolved notices drop the spinner and use
  `aria-live="off"`, and terminal session states do not leave a retry notice
  spinning forever.
- [ ] P2: Add focused coverage for `control-surface-state.ts`:
  six of eight exports currently have no targeted tests. Add a
  co-located `control-surface-state.test.ts` covering:
  `createControlPanelSectionLauncherTab` for the five `sectionId`
  branches + blank-root null-gating on `files` / `git`;
  `resolveWorkspaceScopedProjectId` three-way precedence
  (explicit origin project → origin session's project → null) plus
  trim semantics; `resolveWorkspaceScopedSessionId` precedence
  (preferred session → active session → first-in-project → null);
  `buildControlSurfaceSessionListState` no-search fast-path vs
  search-result-map branch; `mergeOrchestratorDeltaSessions`
  dedup + append-unknowns + `reconcileSessions` identity.
- [ ] P2: Add focused coverage for `git-diff-refresh.ts`:
  `collectGitDiffPreviewRefreshes` has no targeted test. Add a
  co-located `git-diff-refresh.test.ts` that builds a tiny
  `WorkspaceState` with mixed tab kinds (`diffPreview`, others)
  plus a `WorkspaceFilesChangedEvent` and asserts the returned
  refresh list against the four skip conditions (non-diffPreview
  kind, missing `gitDiffRequestKey`, missing `gitDiffRequest`,
  touch-predicate short-circuit). Mirror fixture style from the
  `collectRestoredGitDiffDocumentContentRefreshes` test at
  `App.test.tsx:1489`.
- [ ] P2: Add focused coverage for `source-file-state.ts`:
  both exports untested. `sourceFileStateFromResponse` — assert
  the 13-field state-object mapping against a full `FileResponse`
  and against one with optional fields omitted, including `status`
  and `language`.
  `isSourceFileMissingError` — assert `true` for
  `new Error("File Not Found")`, `new Error("thing was not FOUND")`,
  and a plain `"file not found"` string; `false` for other
  messages.
- [ ] P2: Pin the pending-state tooltip copy for
  `OrchestratorRuntimeActionButton`:
  aria-label regex matchers in `App.test.tsx` cover the three
  labels, but the `isPending: true` `title` tooltips
  (`"Pausing orchestration"` / `"Resuming orchestration"` /
  `"Stopping orchestration"`) are not asserted anywhere. Add one
  assertion per action variant while `isPending: true` to pin the
  copy so it cannot silently regress.
- [ ] P2: Cover the `restartRequired` branch of
  `describeBackendConnectionIssueDetail`:
  `BACKEND_UNAVAILABLE_ISSUE_DETAIL` is already asserted
  indirectly in `backend-connection.test.tsx`, but the
  `isBackendUnavailableError && error.restartRequired` branch —
  which surfaces the server's restart-required message verbatim
  instead of the generic copy — is not confirmed tested. Add a
  small unit test covering all three branches (`restartRequired`
  true, `restartRequired` false, non-backend-unavailable error).
- [ ] P2: Add focused coverage for
  `markdown-diff-segment-stability.ts`:
  the greedy matcher + FNV-1a hash are untested. Distance
  tiebreaks, context-score weights (8 for immediate neighbours, 2
  for second-degree), and collision resolution via
  `getAvailableMarkdownDiffSegmentId` are exactly the invariants
  that regress silently under refactoring. Construct two near-
  identical candidate ranges with deliberately colliding FNV
  hashes and verify tiebreak order by distance / context score,
  plus a collision test that drives the `:stable-N` suffix path.
- [ ] P2: Add focused coverage for `markdown-commit-ranges.ts`:
  4 of 5 exports are untested (`resolveRenderedMarkdownCommitRange`,
  `markdownRangeMatches`, `mapMarkdownRangeAcrossContentChange`,
  `findClosestMarkdownRange`). The existing
  `hasOverlappingMarkdownCommitRanges` tests in
  `DiffPanel.test.tsx:4798` already pull the module in, so sibling
  tests can sit in the same `describe` block. Cases worth pinning:
  4-strategy resolver happy paths + `null` return when nothing
  matches; bounds-check rejection in `markdownRangeMatches`;
  prefix / suffix diff shift cases + straddling-change `null`
  return in `mapMarkdownRangeAcrossContentChange`; and nearest-
  neighbour tiebreak in `findClosestMarkdownRange`.
- [ ] P2: Add focused coverage for `session-slash-palette.ts`:
  25+ exports with no dedicated tests. Heavy integration coverage
  already exists (~14 slash-palette flows in
  `AgentSessionPanel.test.tsx:691-1489`), so this is the lowest
  urgency of the test-coverage tasks, but a pure-function unit
  test of `buildSlashPaletteState` per (agent, commandId) tuple
  would localise failures better than debugging through React
  rendering. Add `panels/session-slash-palette.test.ts` feeding
  canned tuples through `buildSlashPaletteState` and snapshotting
  the item list.
- [ ] P2: Add focused coverage for `markdown-links.ts`:
  the 20+ helpers moved out of `message-cards.tsx` have subtle
  regex/path edges (UNC root restoration, loopback `[::1]`,
  `/C:/` drive-letter-after-slash, `#L42C3` fragment parsing,
  trailing-dot guard for `foo.tex.#L63`). Indirect coverage via
  `MarkdownContent.test.tsx` is thin. Add `markdown-links.test.ts`
  covering: UNC restoration, loopback `http://localhost/Users/...`,
  `file:///C:/...`, `/C:/` drive-letter, `#L42C3` + `#L42`,
  line-suffix stripping `path:42:5`, trailing-dot guard, and
  `isExternalMarkdownHref` for `//cdn.example/x.md`.
- [ ] P2: Add focused coverage for `deferred-render.ts`:
  the seven pure helpers moved out of `message-cards.tsx` include
  `buildMarkdownPreviewText` which strips code fences / link
  syntax / headings / blockquotes / list bullets / backticks via
  regex and produces user-visible preview text. Add
  `deferred-render.test.ts` covering: `buildMarkdownPreviewText`
  (fenced code replaced, links flattened, headings stripped),
  `buildDeferredPreviewText` (line + char limit + ellipsis
  handling), `measureTextBlock` (empty string counts as one
  line), and the min-height clamps in `estimateCodeBlockHeight` /
  `estimateMarkdownBlockHeight`.
- [ ] P2: Add focused coverage for `mermaid-render.ts`:
  the module-level `mermaidRenderQueue` Promise singleton
  serializes Mermaid `initialize`/`render`/reset cycles so
  concurrent renders cannot leak config through Mermaid's
  module-level singleton. This invariant is invisible to
  integration tests (they render one diagram at a time). Add
  `mermaid-render.test.ts` with at minimum a queue-serialization
  test (spy on a fake `mermaid.initialize`/`render`, fire two
  concurrent `renderTermalMermaidDiagram` calls, assert
  initialize-order matches render-completion order and the base
  config is restored between renders), plus pure-helper coverage
  for `clampMermaidDiagramExtent`, `readMermaidSvgDimensions`
  (well-formed / malformed / negative viewBox),
  `getMermaidDiagramFrameStyle` clamp caps, and
  `buildTermalMermaidConfig` palette-vs-match branches.
- [ ] P2: Second import-prune pass over `SessionPaneView.tsx`:
  the first pass (`7e84fe1`) pruned the bulk of unused imports
  from App.tsx / SessionPaneView / WorkspaceNodeView, but
  SessionPaneView still carries three unused locals
  (`pendingPrompts`, `composerInputDisabled`, `composerSendDisabled`
  at 760/851/852) plus a few unused imports. The extraction's
  provenance header explicitly notes a follow-up is expected;
  schedule the second pass once the component stops moving.
- [ ] P2: Add normal-size Mermaid iframe height reserve coverage:
  add a deterministic Mermaid SVG/viewBox case in
  `ui/src/MarkdownContent.test.tsx` proving the `+24` vertical slack is
  applied (for example, an `80px` viewBox height yields a `104px`
  iframe height). Keep the existing huge-viewBox max-height clamp test.
- [ ] P2: Add regression coverage for the delta-persist tombstone restore
  path. The production persist thread lives under `#[cfg(not(test))]` so
  the error-injection test needs either a `#[cfg(test)]` seam in
  `persist_delta_via_cache` that can be primed to fail, or a dedicated
  unit test that exercises a helper extracted from the error branch in
  `src/app_boot.rs`. Assert that after a simulated write failure,
  `inner.removed_session_ids` contains the tombstones that were drained
  into the failed `PersistDelta`, and that the worker re-arms a retry
  without waiting for an unrelated later mutation.
- [ ] P2: Add fake-remote coverage for the "POST response older than SSE"
  gate in `create_remote_session_proxy` + `proxy_remote_fork_codex_thread`.
  Pre-seed `inner.remote_applied_revisions` with a newer revision, then
  have the fake remote respond with an older revision and assert the
  local proxy is NOT refreshed from the POST payload. Pair with an
  `update_existing: true` positive case where the POST revision is
  >= the applied remote revision.
- [ ] P2: Add remote create/fork existing-proxy race coverage:
  pre-seed a local proxy for the same `(remote_id, remote_session_id)`, have
  the fake remote return a fresher
  `CreateSessionResponse { sessionId, session, revision }`, and assert the
  returned response plus local record reflect the fresh payload instead of the
  stale proxy mirror.
- [ ] P2: Add fake-remote regression coverage for the `remote session id
  mismatch` bad-gateway branch in `create_remote_session_proxy` and
  `proxy_remote_fork_codex_thread`. Requires a fake-remote HTTP server
  fixture; today no such fixture exists in `src/tests/`, so the defensive
  validation ships without a direct test. Once that fixture lands, assert
  both routes return `ApiError::bad_gateway` with the
  `remote session id mismatch` message when the fake remote returns a
  `CreateSessionResponse` whose `session.id !== session_id`.
- [ ] P2: Add `adoptCreatedSessionResponse` server-instance-id Vitest
  coverage at the App level: seed `lastSeenServerInstanceIdRef` with one
  id, feed a POST response carrying a different id at a lower revision,
  assert the new session appears in the list and the revision counter
  rewound. The primitive itself is covered by `state-revision.test.ts`,
  but the full adoption wiring through `sessionsRef`, `setSessions`, and
  the workspace layout has no positive test for the restart-rewind
  branch yet. Pair with a same-instance stale response that must preserve
  newer SSE state, and cover both create and fork callers.
- [ ] P2: Add App coverage for successful-send stale response recovery:
  have SSE advance the active session to a newer same-instance revision
  before `sendMessage` resolves with an older `StateResponse`, then assert
  the prompt remains visible, the draft/attachments stay cleared, and the
  active-prompt safety-net poll is armed.
- [ ] P2: Add stale old-server-instance coverage for snapshot adoption:
  adopt instance A, then a lower-revision snapshot from new instance B,
  then resolve a late response from already-seen instance A. Assert the
  late A response is rejected instead of treated as a fresh restart.
- [ ] P2: Assert `serverInstanceId` on create/fork route responses:
  extend `create_session_route_returns_created_response` and
  `codex_thread_fork_route_returns_created_response` to check
  `response.server_instance_id == state.server_instance_id` and that the
  id is non-empty.
- [ ] P2: Add remote create/fork stale-POST revision coverage:
  pre-seed a local proxy as if the remote event bridge already applied a newer
  revision for the same remote session, then have the fake POST response return
  an older `CreateSessionResponse`. Assert the existing newer proxy state is
  retained and no stale `SessionCreated` payload is published.
- [ ] P2: Add remote create/fork response identity validation coverage:
  have fake remotes return mismatched `sessionId` and `session.id` values for
  session create and Codex fork, then assert both paths fail with bad-gateway
  errors instead of localizing the wrong remote session.
- [ ] P2: Add App coverage for stale create-response ordering:
  leave `createSession` pending, apply a fresher SSE `sessionCreated` or
  message delta for the same session, then resolve the create response with a
  lower revision and assert the fresher session state remains intact.
- [ ] P2: Add App coverage for create-response mismatch recovery:
  resolve `api.createSession` with a mismatched `sessionId` and `session.id`,
  then assert recovery resync is requested and no workspace tab is opened for a
  session missing from `sessionsRef`.
- [ ] P2: Strengthen remote rollback content assertions:
  extend `failed_remote_snapshot_sync_restores_session_tombstones` so rollback
  compares full session records and orchestrator instance contents, not only
  restored session-id membership and orchestrator count.
- [ ] P1: Add a direct unit test for `StateInner::collect_persist_delta`:
  construct a `StateInner` with three sessions at distinct mutation stamps
  and one hidden session; seed `removed_session_ids` with one tombstone.
  Call `collect_persist_delta(watermark)` and assert (a) `changed_sessions`
  contains only the visible sessions with stamp > watermark, (b)
  `removed_session_ids` in the returned delta includes the seeded
  tombstone AND any hidden session whose stamp advanced, (c) the returned
  `watermark` equals `inner.last_mutation_stamp` at collection time, (d)
  `metadata.sessions` is empty (metadata-only clone), (e) a second call
  with the returned watermark produces empty `changed_sessions` +
  `removed_session_ids` (idempotent). This is the core of the
  delta-persist refactor and currently has zero regression protection —
  the `#[cfg(test)]` persist path writes full-state JSON so every
  existing persistence test bypasses the production code path.
- [ ] P1: Add an integration-style test for the production persist path:
  open a temp SQLite path, run `AppState::new` with the persist thread,
  issue two `commit_locked` calls touching different sessions, create/fork
  a session that relies on the post-replace re-stamp, and import a
  discovered Codex thread. Wait for the persist thread to drain, read rows
  directly via `rusqlite`, and assert that touched/created/imported rows
  were written while untouched rows were not rewritten. Or, at minimum,
  unit-test `persist_delta_via_cache` directly against a
  `rusqlite::Connection::open_in_memory` using
  `ensure_sqlite_state_schema` and hand-crafted `PersistDelta`s.
- [ ] P2: Add coverage for `StateInner::remove_session_at` and
  `StateInner::retain_sessions`: build a `StateInner`, push sessions via
  `push_session`, run the helper, and assert `removed_session_ids`
  contains the expected ids while kept sessions' stamps are unchanged.
  The raw `record_removed_session` accumulator is tested; the wrappers
  that production deletion paths actually call are not.
- [ ] P2: Document remote-sync revision, rollback, and tombstone invariants:
  add contract comments around `sync_remote_state_inner`,
  `apply_remote_state_if_newer_locked`, and the remote-orchestrator sync
  handoff so future delta-persistence fixes preserve rollback safety.
- [ ] P2: Document orchestrator lifecycle invariants:
  add a lifecycle block to `orchestrators.rs` covering template normalization,
  instance creation, backing sessions, pause/resume/stop, persistence, and
  where transition scheduling is delegated.
- [ ] P2: Document ACP runtime protocol flow:
  add comments for initialize/auth/session load-or-new/config refresh/prompt
  ordering, pending JSON-RPC request ownership, timeout behavior, and
  Gemini/Cursor fallback differences.
- [ ] P2: Document instruction graph traversal semantics:
  describe seed discovery, path normalization, reference sanitization,
  transitive edge policy, skipped directories, and cycle behavior near
  `build_instruction_search_graph`.
- [ ] P2: Add a frontend test proving the self-chained `/api/state`
  safety-net poll does not stack overlapping requests:
  `vi.useFakeTimers()` + a mocked `fetchState` that takes longer than
  the chain boundary, advance time past the next interval, and assert
  `fetchState` was called exactly once per chain hop rather than
  accumulating parallel fires.
- [ ] P2: Add a `publish_snapshot` delivery test: subscribe to
  `state.subscribe_events()` before a mutation and assert the expected
  payload arrives on `state_events` after `commit_locked` through the
  real broadcaster thread. Cover the sync fallback separately by
  disconnecting the broadcaster channel.
- [ ] P2: Add SSE broadcaster latest-only queue coverage:
  publish several large snapshots while the broadcaster is delayed and
  assert superseded snapshots are dropped or overwritten before they can
  accumulate in the queue.
- [ ] P2: Pin `next_mutation_stamp` saturation semantics: the counter
  uses `saturating_add(1)`, but the existing
  `state_inner_next_mutation_stamp_is_strictly_monotonic` only covers
  three increments from zero. Add a one-line case
  (`inner.last_mutation_stamp = u64::MAX; assert_eq!(inner.next_mutation_stamp(), u64::MAX);`)
  so a regression to `wrapping_add` fails the test.
- [ ] P2: Extend the Mermaid dimension-clamp tests with lower-bound cases:
  `ui/src/MarkdownContent.test.tsx` only covers the upper clamp
  (huge viewBox → 4096). Add `viewBox="0 0 -100 -100"` (negative input)
  and `viewBox="0 0 0 0"` (zero input) and assert the rendered widthPx
  and heightPx fall in `[lowerBound, upperBound]`. The regex in
  `clampMermaidDiagramExtent` accepts `[-+]?` signs, so the lower clamp
  is the live contract.
- [ ] P2: Extend `hasOverlappingMarkdownCommitRanges` tests with a
  three-range unsorted case: e.g., `[[0, 5), [10, 20), [3, 12)]`. The
  helper relies on the ascending-by-start sort to detect the overlap;
  a regression that iterated in insertion order would miss it.
- [ ] P2: Add metadata-only state snapshot coverage:
  backend tests should assert `/api/state` omits transcript payloads while
  `GET /api/sessions/{id}` still returns the full transcript. App tests should
  assert a metadata snapshot preserves an already-hydrated active session and
  does not disrupt prompt input or focus.
- [ ] P2: Add session-create persistence contention coverage:
  prove visible session creation does not hold the state mutex while opening
  SQLite, ensuring schema, or committing a transaction.
- [ ] P2: Add rendered Markdown mixed-batch conflict coverage:
  commit two rendered Markdown sections where one range still maps and the
  other conflicts after a document change, then assert no partial apply clears
  the unresolved draft or conflict notice.
- [ ] P2: Add apply-to-disk-version rendered-draft coverage:
  exercise the save-conflict rebase flow with an active contenteditable DOM
  draft only, and again with both committed refs and a newer DOM draft, then
  assert the rebased save includes the latest rendered Markdown edits.
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
- [ ] P2: Move or restore the `SVGElement.prototype.getBBox` /
  `getComputedTextLength` polyfills out of
  `ui/src/panels/DiffPanel.test.tsx:228-243`. The current `beforeEach`
  installs them conditionally on first run and never restores them in an
  `afterEach`, so the patches leak across the entire test file (and any
  test that runs after a DiffPanel test in the same file). Move the
  installation into the shared vitest setup file, or wrap in a proper
  `try`/`finally` with saved originals.
- [ ] P2: Add overlapping rendered Markdown commit-range coverage:
  edit two sections with identical Markdown text (e.g., two `## TODO` headers),
  commit both, and assert the saved content is not garbled. The test should
  trigger `resolveRenderedMarkdownCommitRange` with two commits whose resolved
  byte ranges overlap or are adjacent-but-shifted.
- [ ] P2: Replace `toBeTruthy()` DOM element guards with `not.toBeNull()` or
  `toBeInTheDocument()` in `ui/src/panels/DiffPanel.test.tsx`. There are 16+
  occurrences where `expect(section).toBeTruthy()` guards a queried element
  that is `HTMLElement | null`. `not.toBeNull()` gives a more specific failure
  message and `toBeInTheDocument()` also verifies the element is connected.
- [ ] P2: Add direct unit tests for `mapMarkdownRangeAcrossContentChange`,
  `findClosestMarkdownRange`, and `resolveRenderedMarkdownCommitRange`:
  these pure functions have nontrivial logic (prefix/suffix diffing, closest-
  match scoring, range splicing) tested only indirectly via integration-level
  component tests. Direct unit tests would cover edge cases like empty strings,
  zero-length ranges, content shrinking to empty, and two identical matches
  at different offsets.
- [ ] P2: Strengthen the rendered Markdown committer-registry churn test
  (`ui/src/panels/DiffPanel.test.tsx:886-999`): the current assertion only
  proves sibling DOM node identity is preserved, which is strictly weaker
  than "committers are not unregistered/re-registered while typing". A
  React component can keep its DOM node while its `useEffect` deps still
  change and re-run. Add a direct register/unregister counter via a test
  seam (e.g., wrap `onRegisterRenderedMarkdownCommitter` with a `vi.fn()`
  at the wrapper level, or export a counter for the registration effect),
  and assert call counts stay at zero during a sibling keystroke edit.
- [ ] P2: Return a discriminated result from
  `handleRenderedMarkdownSectionCommits` in `ui/src/panels/DiffPanel.tsx`:
  the boolean return value currently overloads "applied cleanly", "no-op
  (no content change)", and "conflict; keep drafts dirty" into two states,
  which requires reading the caller to understand the contract. Return
  `"applied" | "no-op" | "conflict"` (or rename to
  `tryApplyRenderedMarkdownSectionCommits` with a doc comment) so callers
  can distinguish successful application from a silent no-op.
- [ ] P2: Reorder `collectSectionEdit` no-op branch in
  `ui/src/panels/DiffPanel.tsx`: on the "edit reverted to original" path
  the function clears `hasUncommittedUserEditRef`, `draftSegmentRef`, and
  `draftSourceContentRef` before calling `onDraftChange`. If the parent
  transitively re-reads committers during the same flushSync batch, it
  could see "no draft" in the refs while the segment is still listed in
  `renderedMarkdownDraftSegmentIdsRef`. Move `onDraftChange` before the
  ref clears for symmetry with the other branches.
- [ ] P2: Rename and split the Rust
  `git_diff_document_enrichment_note_uses_structured_error_kind` test
  in `src/tests.rs`: the test now also asserts the new status-code
  fallback path for untagged `BAD_REQUEST`/`NOT_FOUND`, which is not
  structured-kind driven. Rename to
  `git_diff_document_enrichment_note_fallback_for_bad_request_and_not_found`
  and keep a separate test that proves a BAD_REQUEST with size-suggestive
  message text still returns the generic fallback (not the size-specific
  note), so the "kind over message text" invariant has dedicated coverage.
- [ ] P2: Loosen the exact-string assertion in the rendered Markdown HTML
  paste sanitizer test (`ui/src/panels/DiffPanel.test.tsx:2178-2265`):
  the final `onSaveFile` assertion pins `"# Draft document\n\nNew section\n\nVisible linksafe text\n"`
  verbatim, which ties the security test to serializer whitespace
  behavior. Replace with a property-based assertion that the saved
  payload does not contain `onclick|onload|srcdoc|javascript:|<script|<svg|<iframe`
  (or document the exact-string contract explicitly so future whitespace
  edits update this test).
- [ ] P2: Extract a shared Monaco mock helper for `ui/src/App.test.tsx`,
  `ui/src/panels/DiffPanel.test.tsx`, and `ui/src/panels/SourcePanel.test.tsx`:
  the three files now duplicate `vi.mock("./MonacoDiffEditor", ...)` and
  `vi.mock("./MonacoCodeEditor", ...)` with subtle shape differences
  (status callback payloads, scroll-handle API). Move the baseline mock
  into a shared module (or `test-setup.ts` with overrides) so future
  real-component changes update one place.
- [ ] P2: Replace the ordering-dependent `mockImplementationOnce` chain
  in the "ignores stale manual Git diff responses" test
  (`ui/src/App.test.tsx:2070-2218`): the test relies on the first
  `fetchGitDiff` call returning `staleDiffDeferred.promise` and the
  second returning `currentDiffDeferred.promise`. A future code change
  that adds any intermediate fetch silently swaps the mapping. Replace
  with `mockImplementation((req) => ...)` that keys off a
  request-correlated field (e.g., call counter + filePath) so the
  deferred mapping is explicit. Also add
  `expect(fetchGitDiffSpy).toHaveBeenCalledTimes(2)` after the stale
  resolve so the test pins the "exactly two fetches" guarantee that
  the `attemptedGitDiffDocumentContentRestoreKeysRef.current.add(requestKey)`
  fix at `App.tsx:6020` actually provides — today the Monaco-content
  assertion is satisfied by the separate version-counter guard and
  would still pass even if the dedupe fix regressed.
- [ ] P2: Memoize `srcDoc` in `MermaidDiagram`
  (`ui/src/message-cards.tsx`): `buildMermaidDiagramFrameSrcDoc(renderState.svg)`
  returns a new string on every render. Any parent re-render reloads
  the iframe because React sees a new `srcDoc` prop identity. Wrap the
  computation in `useMemo` keyed on `renderState.svg` so the iframe is
  stable across unrelated parent re-renders.
- [ ] P2: Short-circuit the restored-document-content scan in
  `ui/src/App.tsx:3906-4015` when every `diffPreview` tab already has
  `documentContent`. The scan now runs on every `workspace.panes`
  change; in workspaces with many diff tabs it is O(panes × tabs) per
  `setWorkspace`. An early-return when all tabs are fully hydrated
  keeps the fix for late-hydration restore without adding a per-update
  cost in common cases.
- [ ] P2: Move `useStableEvent` from `ui/src/panels/DiffPanel.tsx` into
  a shared hooks module (`ui/src/hooks.ts` or similar). The primitive
  is general (stable callback ref + `useLayoutEffect` publish window),
  and a future panel that recreates it locally will miss the new
  `flushSync` layout-phase comment and regress the same subtle bug.
- [ ] P2: Tighten the Mermaid sandbox test's `onload` substring
  assertion in `ui/src/MarkdownContent.test.tsx:245-261`:
  `expect.stringContaining("onload")` passes on any `onload` substring,
  including typos like `onloadxx`. Replace with
  `expect(frame.getAttribute("srcdoc")).toContain('onload="alert(1)"')`
  (full attribute match) or drop the assertion entirely in favor of the
  already-present `queryByTestId("mermaid-svg")).not.toBeInTheDocument()`
  which is the real isolation invariant.
- [ ] P2: Add live-updates.ts `sessionCreated` unit tests:
  add three tests in the `applyDeltaToSessions` suite — (1) session is
  appended when `sessionIndex === -1`, (2) session is replaced in place
  when it already exists, (3) `needsResync` is returned when
  `delta.session.id !== delta.sessionId`. Mirror the coverage pattern of
  other delta arms.
- [ ] P2: Add App-level tests for lazy session hydration:
  stub `api.fetchSession`, render a workspace with an active session
  marked `messagesLoaded: false`, assert (a) one-shot `fetchSession`
  fires, (b) a repeat render does not re-fetch, (c) a mismatched-id
  response triggers `requestActionRecoveryResyncRef`, (d) a stale
  (lower-revision) response is rejected without marking the session as
  hydrated.
- [ ] P2: Add 404 tests for `GET /api/sessions/{id}`:
  `get_session_route_returns_not_found_for_unknown_id` and
  `get_session_route_returns_not_found_for_hidden_session`. The hidden
  case is especially important — `find_visible_session_index` is the
  load-bearing invariant that prevents hidden Claude spares from leaking
  through the public route.
- [ ] P2: Add Rust coverage for `apply_remote_delta_event_locked::SessionCreated`:
  `remote_session_created_delta_creates_local_proxy_and_publishes_local_delta`
  — feed a remote `SessionCreated` with a fresh remote session id, assert
  a local proxy appears with remapped project id, the outbound local
  `SessionCreated` carries the local id, the revision bumps. Add an
  id-mismatch variant that returns the `anyhow!` error.
- [ ] P2: Add conflict-batch test for `handleRenderedMarkdownSectionCommits`:
  exercise the new boolean-`false` branch — make two sibling rendered
  Markdown edits, trigger a document refresh that unmaps one range,
  commit the batch, and assert (a) save-error banner visible,
  (b) both drafts still dirty, (c) `onSaveFile` not called. This pins
  the atomicity invariant the bug entry claims.
- [ ] P2: Gate the oversized-Mermaid assertion with `waitFor`:
  `ui/src/panels/DiffPanel.test.tsx:1265-1332` asserts
  `expect(mermaidRenderMock).not.toHaveBeenCalled()` synchronously after
  `render()`, but Mermaid is async-gated by a `useEffect`. Add an
  `await waitFor(() => expect(screen.queryByTestId("mermaid-frame")).not.toBeInTheDocument())`
  first so the effect has a chance to not run before the mock assertion.
- [ ] P2: Strengthen `create_session_refreshes_agent_readiness_cache`:
  the updated delta assertion checks ids but not session body fields.
  Add at least one identity-confirming assertion (e.g.,
  `assert_eq!(session.name, "Test Codex Session")`) so a regression
  that emits the wrong session body with a matching id fails.
- [ ] P2: Replace brittle `{ overwrite: undefined }` matcher in the
  save-options test (`ui/src/App.test.tsx`):
  `toHaveBeenNthCalledWith(1, ..., { baseHash, overwrite: undefined, ... })`
  treats missing and `undefined` as equal in Vitest deep-equality. A
  future refactor that conditionally spreads `overwrite` still passes
  even if the intent ("first save does not send overwrite") breaks.
  Use `expect.objectContaining({ baseHash: "..." })` plus
  `expect(saveFileSpy.mock.calls[0][2]).not.toHaveProperty("overwrite")`.
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
