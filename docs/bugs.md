# Bugs & Known Issues

This file tracks reproduced, current issues. Resolved work, speculative refactors,
and cleanup notes do not belong here.

## Active Repo Bugs

Also fixed in the current tree, not re-listed below:

- **Stale height estimate on tab switch causing blank area** — `VirtualizedConversationMessageList` now enters a post-activation measuring phase when it transitions inactive → active (or mounts directly in the active state with messages). The wrapper is hidden via `visibility: hidden` while currently-visible slots report their first `ResizeObserver` measurements, then the completion check writes a final scrollTop and reveals. A 150 ms timeout fallback guarantees the wrapper is never stuck hidden. Scroll-restore on activation now lands in the correct place even when messages arrived while the tab was inactive.
- **Steady-state 1-2 px shake in active session panels** — `handleHeightChange` now rounds `getBoundingClientRect().height` to integer pixels before storage, and all three scrollTop-to-bottom writes (re-pin `useLayoutEffect`, `handleHeightChange` shouldKeepBottom branch, `getAdjustedVirtualizedScrollTopForHeightChange` branch) are wrapped in `Math.abs(current - target) >= 1` no-op guards. Subpixel drift in successive `getBoundingClientRect` reads no longer crosses the 1-pixel commit threshold, and no-op scrollTop writes no longer trigger scroll-event → reflow → ResizeObserver cascades.

## Rendered Markdown drafts can be overwritten by file-change lifecycle handling

**Severity:** High - external file-change, reload, or delete handling can treat a rendered Markdown draft as clean while the user's edit still exists only in contentEditable DOM state.

Rendered Markdown edits now stay local to the editable section until blur/save/explicit commit. The file-change lifecycle still primarily checks `editValue !== latestFile.content` when deciding whether the buffer is dirty. If a workspace file-change event, watcher refresh, or delete event arrives while a rendered Markdown section has an uncommitted DOM draft, the lifecycle path can reload or replace the file as though no local edit exists.

**Current behavior:**
- `editValue` can remain equal to the latest file content while a rendered Markdown draft is active.
- File-change handling can classify that state as clean.
- A refresh/delete/rebase can unmount or replace the section before the draft is flushed.

**Proposal:**
- Track rendered draft activity in a ref or per-section dirty set that lifecycle handlers can read synchronously.
- Flush `commitRenderedMarkdownDrafts()` before reload/delete/rebase handling, or block those paths with a conflict notice until drafts are committed or cancelled.
- Add a regression test where an uncommitted rendered Markdown draft survives an external file-change event.

## Stripped diff preview document content is not restored after async workspace layout hydration

**Severity:** High - diff preview tabs restored from the persisted workspace layout can reopen without `documentContent`, leaving rendered Markdown previews stuck in raw/read-only mode.

Diff preview document content is stripped before workspace layout persistence, which is correct for storage size. The restore scan that re-fetches missing document content currently runs only once on mount. If the app later applies a saved layout from `fetchWorkspaceLayout()`, that async-hydrated workspace can contain stripped diff preview tabs that the one-time scan never sees.

**Current behavior:**
- The initial restore scan can run before server workspace layout hydration finishes.
- Hydrated diff preview tabs can have `documentContent` stripped.
- Those tabs are not automatically re-fetched, so rendered Markdown mode can be unavailable until manual reopen/reload.

**Proposal:**
- Run the restore scan after workspace hydration as well, keyed on workspace/layout readiness.
- Track in-flight or already-restored request keys so the scan cannot create duplicate fetch loops.
- Add a startup regression test with a persisted diff preview tab that has a `gitDiffRequest` but no `documentContent`.

## Markdown source-link normalization drops Windows UNC roots

**Severity:** Medium - Markdown links under Windows network workspaces can resolve to non-absolute paths.

`normalizeJoinedMarkdownPath` handles POSIX roots and drive-letter Windows paths, but not UNC roots such as `\\server\share\docs`. Splitting the path drops the leading UNC authority markers, so a joined link can become `server\share\...` instead of preserving `\\server\share\...`.

**Current behavior:**
- UNC workspace roots are split like normal path segments.
- The leading network-root prefix is lost.
- Source-link open requests can receive a relative-looking path on Windows.

**Proposal:**
- Add an explicit UNC branch before drive-letter handling.
- Preserve the `\\server\share\` prefix and prevent `..` normalization from escaping above the share root.
- Add a Markdown link regression test for a UNC `workspaceRoot`.

## Fenced Markdown code block changes can split the rendered diff section

**Severity:** Medium - a code-fence body edit can render as invalid Markdown because the changed segment may start after the opening fence.

Rendered Markdown diff segmentation tracks fence blocks, but changed ranges that touch a fenced block currently expand forward to the closing fence without also expanding backward to the opener. If only an interior line changes, the opener can be emitted as a normal section while the changed section contains the code body and closing fence on its own. Each section is rendered independently, so the changed half is no longer valid fenced Markdown.

**Current behavior:**
- Anchors inside a changed non-Mermaid fenced block can split the fence opener away from the changed body.
- The rendered diff section can display code as prose or produce unstable edit semantics.
- Existing Mermaid-specific handling does not cover ordinary fenced code blocks.

**Proposal:**
- Treat any changed fenced block as an atomic section by expanding both start and end to the full fence block.
- Alternatively, filter anchors inside changed fence blocks before segment emission.
- Add a regression test where only an interior line of a non-Mermaid fenced block changes.

## Nested unordered-list indentation changes can disappear in rendered Markdown diff

**Severity:** Medium - rendered Markdown diff normalization can treat structurally different nested list items as identical.

`normalizeRenderedMarkdownLineForDiff` normalizes unordered list markers to `- ${text}` and drops indentation depth. In Markdown, `- item` and `  - item` can represent different list structure. If a change only moves an item into or out of a nested list, the rendered diff comparison can decide the lines are equivalent even though the rendered document structure changed.

**Current behavior:**
- Unordered list indentation is removed from the rendered comparison key.
- Nested-list promotions or demotions can be hidden from the rendered diff.
- Users may see no rendered change even though the Markdown DOM changes.

**Proposal:**
- Preserve nesting depth in the comparison key for unordered list items.
- Or compare list structure from a Markdown AST instead of using line-only normalization.
- Add a regression test for `- parent\n  - child` changing to `- parent\n- child`.

## Rendered Markdown downstream-draft regression commits before exercising the risky path

**Severity:** Medium - the regression test intended to protect an uncommitted downstream draft can pass after the draft has already been committed by blur.

The test edits section two, then focuses section one to make an upstream edit. That focus transition can blur section two and trigger its commit path before the upstream line-count shift happens. The test then no longer proves that a purely local contentEditable draft survives a structural segment shift.

**Current behavior:**
- The helper focuses each edited section.
- Focusing the upstream section can blur and commit the downstream draft first.
- The test can pass while the actual uncommitted-DOM-draft scenario remains uncovered.

**Proposal:**
- Add a variant that keeps the downstream draft local while upstream content or segments change.
- Or assert explicitly that no blur/commit occurred before the upstream offset shift.
- Keep the existing committed-draft coverage as a separate, clearly named case.

## SourcePanel Markdown mode reset test starts in code mode

**Severity:** Low - the reset test can pass without proving that non-Markdown files reset Markdown preview mode.

The test clicks `Code` before rerendering the panel with a non-Markdown file. If the reset behavior broke, the assertion would still pass because the mode was already code before the file-type transition.

**Current behavior:**
- The test moves from Markdown preview/split behavior back to code manually.
- The later non-Markdown rerender does not have to perform the reset for the assertion to pass.
- The test name overstates the behavior being verified.

**Proposal:**
- Leave the panel in `Preview` or `Split`, rerender to a non-Markdown file, then rerender back to Markdown and assert the mode starts in code.
- Keep a separate assertion for the manual Code button if that behavior is still needed.

## Oversized Markdown enrichment note lacks response-level coverage

**Severity:** Low - the user-visible API behavior for oversized Markdown enrichment is not covered by an integration-style test.

The oversized Markdown enrichment note is tested through lower-level reader/helper calls. The actual behavior users depend on is that `load_git_diff_for_request` catches the enrichment failure, returns the raw diff, omits `documentContent`, and serializes `documentEnrichmentNote` in the response.

**Current behavior:**
- Low-level helper behavior is covered.
- The API response path can regress without a direct test.
- Camel-case response serialization for this degraded path is not asserted.

**Proposal:**
- Add a `load_git_diff_for_request` or HTTP JSON test with an oversized changed Markdown file.
- Assert the response contains raw diff text, `documentEnrichmentNote`, and no `documentContent`.

## First-measurement commit is skipped when the estimate already matches

**Severity:** Low - a narrow edge case where a slot's first ResizeObserver report happens to round within 1 px of its `estimateConversationMessageHeight` leaves `messageHeightsRef` without an entry for that slot, preventing the measuring-phase completion check from taking the fast path.

`handleHeightChange` computes `previousHeight = messageHeightsRef.current[messageId] ?? estimateConversationMessageHeight(...)` and early-returns when `Math.abs(previousHeight - roundedHeight) < 1`. On the FIRST measurement for a newly-visible slot, `previousHeight` is the estimate. If the real measured height happens to land within 1 px of the estimate, the early-return fires and the ref stays `undefined` for that slot. The measuring-phase completion check at `visibleMessages.every(message => messageHeightsRef.current[message.id] !== undefined)` then stays false until the slot's height actually changes, forcing the measuring phase to rely on the 150 ms fallback instead of the fast completion path.

**Current behavior:**
- First measurement is skipped when the rounded real height is within 1 px of the estimate.
- `messageHeightsRef.current[messageId]` remains `undefined` after a "successful" first `ResizeObserver` tick.
- The measuring-phase completion check cannot conclude `allMeasured` for that slot.
- The wrapper stays hidden until the 150 ms timeout fallback fires.

**Proposal:**
- Track whether this is the first measurement for the slot and always commit the first one:
  ```ts
  const hadPreviousMeasurement = messageHeightsRef.current[messageId] !== undefined;
  const previousHeight = messageHeightsRef.current[messageId] ?? estimateConversationMessageHeight(...);
  if (hadPreviousMeasurement && Math.abs(previousHeight - roundedHeight) < 1) {
    return;
  }
  messageHeightsRef.current[messageId] = roundedHeight;
  ```
- Add a regression test that estimates a message at height N, measures it at N+0.4 (rounds to N), and asserts the ref has an entry after the measurement.

## Rendered Markdown section double-mounts on first render via renderResetVersion

**Severity:** High - every editable Markdown diff section unmounts and remounts its `MarkdownContent` on its first render, discarding the ~8-dep `useMemo` and re-parsing Markdown twice.

`EditableRenderedMarkdownSection` in `ui/src/panels/DiffPanel.tsx` holds a `renderResetVersion` state value used as `key={renderResetVersion}` on the child `MarkdownContent`. A reset effect `useEffect(() => { hasUncommittedUserEditRef.current = false; setRenderResetVersion((c) => c + 1); }, [segment.markdown])` bumps the state on every mount because `useEffect` always runs after the initial commit. Render pass 1 uses `key=0`; the effect fires, schedules a state update; render pass 2 uses `key=1`; React unmounts the `key=0` tree and mounts a fresh `key=1`. For a diff with many editable sections this re-introduces the "double commit on mount" class of bug that the `useLayoutEffect → useEffect` refactor in `message-cards.tsx` was specifically built to avoid.

**Current behavior:**
- First mount of every editable rendered Markdown section runs a full `MarkdownContent` mount twice in a row.
- The memoized ReactMarkdown tree is rebuilt from scratch on the second commit.
- Typing latency and scroll restoration cost are higher than necessary for every new diff open.

**Proposal:**
- Guard the reset with a `previousSegmentMarkdownRef` so the state only increments when `segment.markdown` actually changes after the first render.
- Or replace the state+key remount pattern with a ref + imperative `innerHTML` reset so React does not own the subtree during a cancel/reset.
- Add a regression test that counts `MarkdownContent` mount lifecycles for a new section and asserts exactly one.

## Rendered Markdown drafts typed during a save are silently dropped

**Severity:** High - removing `isSaving` from `canEditRenderedMarkdown` reopened a subtler variant of the "section draft dropped mid-save" bug: typing during `handleSave`'s await window is not captured on the post-save reconciliation path.

`handleSave` in `ui/src/panels/DiffPanel.tsx:653-710` now calls `commitRenderedMarkdownDrafts()` at the top (flushing any pre-save draft into `editValue`), awaits `onSaveFile`, then reads `editValueRef.current` again to detect `hasLocalEditsAfterSaveStarted`. The problem is that mid-save keystrokes call `handleDraftChange` → `handleRenderedMarkdownSectionDraftChange`, which only flips `hasRenderedDraftActive` — it deliberately does NOT propagate to `editValueRef`. So `editValueRef.current` after the await equals `currentEditValue`, the reconciliation path short-circuits, and `setMarkdownEditContentState(savedContent)` overwrites the panel buffer. The uncommitted DOM draft persists in the specific section via `hasUncommittedUserEditRef`, but the post-save `documentContent` reset effect can then clear `hasRenderedDraftActive`, disabling the Save button while the uncommitted content is still sitting in the DOM. On a watcher-driven `documentContent` refresh, the edited section may be absorbed into a merged segment and unmount, losing the DOM draft entirely.

**Current behavior:**
- `canEditRenderedMarkdown` no longer checks `isSaving`, so users can type during saves.
- `handleSave` only flushes committers BEFORE the await, not AFTER.
- Mid-save keystrokes live only in `hasUncommittedUserEditRef` + contentEditable DOM.
- `hasLocalEditsAfterSaveStarted` is always false on the rendered-edit path.
- A subsequent `documentContent` refresh can unmount the section and lose the DOM draft.

**Proposal:**
- Flush committers a second time after the await: `await onSaveFile(...); commitRenderedMarkdownDrafts();` before reading `editValueRef.current`.
- Add a regression test that uses a deferred `onSaveFile` mock, types into the section after save starts, resolves the deferred, and asserts the later edit is preserved in `pendingEditValueRef` and saved on the next save.

## Git diff document enrichment note classifies errors by substring-matching error text

**Severity:** Medium - `git_diff_document_enrichment_note` couples its classification to the exact wording of internal error messages, so a future reword silently drops the user-facing note.

`src/api.rs:4147-4164` does `error.message.to_lowercase().contains("exceeds the 10 mb read limit")` and `"changed to a symlink"` to decide which note to emit. The matched strings are produced elsewhere in `api.rs` as format literals. A well-intentioned reword of either error (e.g., bumping the limit from "10 MB" to "20 MB", or rewriting to "is larger than the 10 MB limit") silently drops the user-facing note while the degraded state still happens. The only guard is a pinning test with the current wording, which does not force the classifier to stay in sync with the producers.

**Current behavior:**
- The classifier greps free-form localized error messages with `to_lowercase().contains(...)`.
- A reword of the producer-side error silently breaks the note-emission path.
- The pinning test asserts only the current wording, not the classifier-producer contract.

**Proposal:**
- Introduce an `ApiErrorKind` enum (or `code: &'static str` field on `ApiError`) with variants like `GitDocumentTooLarge` and `GitDocumentBecameSymlink`, and match on that in `git_diff_document_enrichment_note`.
- Or have the reader callers return `Result<String, GitDocumentReadError>` with a dedicated error enum and map it to both `ApiError` and the note at the call site so there is a single source of truth.

## Git diff document enrichment note is missing for several degraded-preview paths

**Severity:** Medium - several error shapes that degrade Markdown preview return `document_content.is_none()` with no accompanying note, so the UI drops silently to plain-text diff with no explanation.

`git_diff_document_enrichment_note` covers only two patterns — oversized read and worktree-symlink-swap. These NOT_FOUND/BAD_REQUEST error shapes also produce `document_content.is_none()` but emit no note:
- Non-UTF-8 Markdown (`"{label} is not valid UTF-8"`)
- Worktree symlink target is not a file
- Worktree path is not a file
- Worktree path not found
- `{label} not found: {spec}` from `read_git_spec_text`
- `git {label} not found`

**Current behavior:**
- Any of these errors leaves `document_content` and `document_enrichment_note` both unset.
- The UI degrades to plain text diff view with no visible reason.
- Users have no feedback distinguishing these cases from "non-Markdown file".

**Proposal:**
- Extend the classifier (or the structured-error refactor in the adjacent bug) to produce a note for each case.
- Add regression tests asserting `document_enrichment_note` is set for each failure path.

## Git diff enrichment failures over 500 discard the entire raw diff

**Severity:** Medium - an I/O hiccup during Markdown enrichment loses the raw diff that the user was trying to view.

`load_git_diff_for_request` at `src/api.rs:3525-3533` matches only `StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND` in the enrichment-error tolerance arm, letting everything else (transient I/O errors from `read_git_worktree_text`, unexpected git failures, etc.) propagate as a hard error that fails the whole `/api/git/diff` request. The raw diff text has already been computed successfully by that point and would be perfectly useful on its own, but it is thrown away because Markdown enrichment tripped.

**Current behavior:**
- A 500 from the enrichment layer fails the entire `/api/git/diff` response.
- The already-loaded raw diff text is discarded.
- The user gets an error for the whole diff tab instead of a degraded-but-useful view.

**Proposal:**
- Treat the 500 path the same as the BAD_REQUEST / NOT_FOUND path: log, populate `document_enrichment_note` with a generic "Rendered Markdown is unavailable due to a read error.", set `document_content = None`, and return the raw diff.
- Add a regression test that simulates an I/O error from `read_git_worktree_text` and asserts the diff response still contains the raw diff text plus the enrichment note.

## Committer-registry thrashes on every DiffPanel state update

**Severity:** Medium - every DiffPanel state change (including per-keystroke `setHasRenderedDraftActive` toggles) forces all N editable Markdown sections to unregister and re-register their committers.

`handleRenderedMarkdownSectionDraftChange` and `commitRenderedMarkdownDrafts` in `ui/src/panels/DiffPanel.tsx` are declared as plain `function`s inside the component, so their identity changes on every render. That identity change propagates through `MarkdownDiffView`'s `handleRenderedMarkdownSectionDraftChange = useCallback(..., [onRenderedMarkdownSectionDraftChange])` into each `EditableRenderedMarkdownSection`'s `collectSectionEdit = useCallback([canEdit, onDraftChange, segment])`, which invalidates the committer registration `useEffect([collectSectionEdit, onRegisterCommitter])`. The cleanup deletes the old committer from the `renderedMarkdownCommittersRef` Set and the effect re-adds a new one. For a diff with many editable sections this happens on every keystroke because `setHasRenderedDraftActive(true)` triggers a DiffPanel re-render.

**Current behavior:**
- Parent handler identities change on every render.
- `collectSectionEdit` invalidates because `onDraftChange` (and `commitRenderedMarkdownDrafts`) got a new identity.
- The registration `useEffect` does cleanup-then-re-register for every editable section on every keystroke.
- The `renderedMarkdownCommittersRef` Set churns proportional to `number of editable sections × keystrokes`.

**Proposal:**
- Stabilize the parent handlers via ref-forwarded `useCallback`s so their identity is stable across renders:
  ```ts
  const draftHandlerRef = useRef(handleRenderedMarkdownSectionDraftChange);
  useEffect(() => { draftHandlerRef.current = handleRenderedMarkdownSectionDraftChange; });
  const stableDraftHandler = useCallback((segment, nextMarkdown) => {
    draftHandlerRef.current(segment, nextMarkdown);
  }, []);
  ```
- Apply the same pattern to `commitRenderedMarkdownDrafts`, `handleRenderedMarkdownSectionCommits`, and `handleOpenMarkdownSourceLink`.
- Add a performance regression test that mounts a N-section diff, types a character, and asserts the committer-set delete/add count is O(1) per keystroke, not O(N).

## documentContent reset effect clears dirty flag while an uncommitted DOM draft still exists

**Severity:** Low - the Save button can become disabled while an uncommitted rendered Markdown draft is still sitting in the contentEditable DOM, causing transient button-state confusion.

The `[documentContent]` reset effect in `ui/src/panels/DiffPanel.tsx:308-316` clears `markdownEditContent` and `hasRenderedDraftActive` when `editValueRef.current === currentFile.content`. Because drafts no longer propagate to `editValue` per keystroke, an uncommitted contentEditable draft can sit in the DOM while the reset effect concludes "nothing is dirty" and disables the Save button. Flush/Escape/navigation still recovers the draft, so no silent data loss in the simple case — but content-hash segment IDs only shrink the window vs positional IDs, they do not close it. A post-save file-watcher refresh that rebuilds segments can still absorb the edited section into a merged segment and lose the DOM draft entirely.

**Current behavior:**
- The effect clears the flag based solely on `editValue === currentFile.content`.
- Uncommitted DOM drafts in `hasUncommittedUserEditRef` are not considered.
- The Save button becomes misleadingly disabled while uncommitted content is still present.
- A subsequent segment rebuild can unmount the section and drop the draft.

**Proposal:**
- Before clearing `hasRenderedDraftActive`, check whether any committer in `renderedMarkdownCommittersRef` returns a non-null commit. If so, preserve the flag.
- Or call `commitRenderedMarkdownDrafts()` at the top of the reset effect so pending drafts are flushed before the flag is cleared.

## Git diff document enrichment symlink branch is Unix-only dead code on Windows

**Severity:** Low - the `"changed to a symlink"` pattern in `git_diff_document_enrichment_note` is structurally unreachable on Windows.

`open_worktree_file` on `#[cfg(not(unix))]` uses `fs::File::open` directly and never produces the `"changed to a symlink"` error string. The enrichment-note match arm for that substring is dead code on Windows (a P0 platform). The existing comment at the function describes a Unix-specific symlink-swap race, so the intent is intentional, but the current code carries a Windows-unreachable branch with no `#[cfg]` annotation or explanatory comment.

**Current behavior:**
- The symlink branch in `git_diff_document_enrichment_note` cannot fire on Windows.
- No `#[cfg(unix)]` annotation or comment documents the platform constraint.
- Future maintainers may think the branch is cross-platform.

**Proposal:**
- Annotate the symlink branch with `#[cfg(unix)]` or add a comment explaining the Unix-only scope.
- Document the Windows single-user threat model assumption at the function level.

## Rendered Markdown draft dirty state is shared across sections

**Severity:** High - one rendered Markdown section can clear the dirty flag while another section still has an unsaved local draft.

`DiffPanel` tracks rendered Markdown draft activity with a single `hasRenderedDraftActive` boolean. Multiple rendered sections can be editable at the same time, so a blur or no-op commit from one section can set the flag back to false even while another section still has dirty DOM content.

**Current behavior:**
- Draft activity is stored as one panel-wide boolean.
- Any section can clear that boolean.
- The Save button and saved/dirty state can report clean while another section still contains an uncommitted draft.

**Proposal:**
- Track rendered draft activity per segment id, or keep a set/counter of dirty rendered sections.
- Derive `isDirty` from the aggregate draft state instead of the last section event.
- Add a regression test with two dirty rendered sections where one section blurs cleanly while the other remains dirty.

## Repeated Markdown diff chunks still have order-sensitive segment ids

**Severity:** High - repeated identical Markdown blocks can still remount and drop draft state when an earlier duplicate is inserted or removed.

`assignMarkdownDiffSegmentIds` hashes segment content and disambiguates duplicate chunks with an occurrence count. That is stable for unique chunks but still order-sensitive for repeated identical blocks. If a document has repeated sections, an upstream insert/delete can shift occurrence counts for later identical sections and change their React keys.

**Current behavior:**
- Segment ids include a content hash plus an occurrence index.
- Repeated identical chunks depend on render order.
- Later repeated chunks can remount after unrelated upstream line-count changes.

**Proposal:**
- Include stable structural context in the segment identity, or preserve prior ids for unchanged segments through a hash-to-id map.
- Add a duplicate-block regression fixture that proves a specific downstream repeated chunk keeps its id across an upstream line-count shift.

## Markdown enrichment note is hidden in raw patch fallback

**Severity:** Medium - users can lose the explanation for why rendered Markdown mode is unavailable.

The backend now returns `documentEnrichmentNote` when Markdown enrichment cannot provide full before/after document content. The frontend only surfaces that note through the structured Markdown preview path, so if diff parsing falls back to raw patch mode the note is dropped.

**Current behavior:**
- `documentEnrichmentNote` is available on the diff preview tab.
- Raw patch fallback can render without displaying the note.
- Users see patch mode but not the reason rendered Markdown is unavailable.

**Proposal:**
- Render `documentEnrichmentNote` independently of structured preview availability, such as in the diff header/status area.
- Add a regression test where Markdown enrichment returns a note but structured preview is unavailable.

## MarkdownContent line numbers no longer default to line 1

**Severity:** Medium - existing callers that pass `showLineNumbers` without a base line can lose their gutter.

`MarkdownContent` now treats an omitted `startLineNumber` as unknown. That is correct for omitted diff patch placeholders, but it also changes the broader component contract: callers that only enable `showLineNumbers` no longer get line numbers starting at 1.

**Current behavior:**
- `showLineNumbers` alone does not guarantee gutter markers.
- Existing document-view callers can lose line numbers unless they also pass `startLineNumber`.
- The omitted-line use case and normal document use case are conflated.

**Proposal:**
- Restore the default `startLineNumber = 1` when `showLineNumbers` is true for normal Markdown rendering.
- Add a separate unknown-line mode for diff placeholders that should suppress gutter markers.
- Cover both behaviors in `MarkdownContent` tests.

## Git diff refresh versions can reset while a stale refresh is in flight

**Severity:** Medium - closing and quickly reopening a diff tab can allow an old refresh response to be accepted.

The cleanup effect deletes `gitDiffPreviewRefreshVersionsRef` entries as soon as the request key disappears from the workspace. If a tab is closed while a refresh is in flight and then reopened before the old request finishes, the counter has been reset and the old response may look current.

**Current behavior:**
- Refresh version entries are deleted when a request key is absent from the workspace.
- In-flight requests can outlive the tab that started them.
- A close/reopen cycle can reset the guard that rejects stale responses.

**Proposal:**
- Keep refresh versions monotonic for the process lifetime, or track a separate open-generation token.
- Add a regression test for close/reopen while an old diff refresh promise resolves late.

## Rendered Markdown line-count-shift test emits act warnings

**Severity:** Medium - the regression test passes but leaves React scheduler warnings in the suite output.

The new line-count-shift regression test drives edit, blur, and save transitions without fully wrapping the async React state updates. The test currently passes, but repeated `act(...)` warnings make real regressions easier to miss and make the test more scheduler-sensitive.

**Current behavior:**
- The test executes edit/blur/save interactions that trigger async state updates.
- React emits `act(...)` warnings during the test run.
- Warning noise is mixed into otherwise passing output.

**Proposal:**
- Wrap the edit/blur/save sequence in `await act(async () => { ... })`, or use `userEvent` with explicit waits around committed state transitions.
- Keep the same behavioral assertion after the warning is removed.

## documentEnrichmentNote is normalized as an identifier

**Severity:** Low - a user-facing message can be trimmed or otherwise treated like an internal id.

`createDiffPreviewTab` normalizes `documentEnrichmentNote` through `normalizeWorkspaceIdentifier()`. The note is user-facing text, not an identifier, so identifier-style normalization can mutate the exact message and may not be appropriate for future multi-line notes.

**Current behavior:**
- `documentEnrichmentNote` is passed through an id-normalization helper.
- Leading/trailing whitespace is trimmed.
- The helper name and semantics do not match the field's purpose.

**Proposal:**
- Preserve the note verbatim except for converting missing/empty values to null if needed.
- Use a text-specific helper so future callers do not assume the note is an identifier.

## Markdown line-number measurement can paint stale gutter positions

**Severity:** Low - the gutter can briefly show old marker positions before geometry is recomputed.

Markdown line-number measurement now runs in a passive effect. Because it reads DOM geometry, the browser can paint with stale marker positions before the scheduled measurement updates them, especially when rendered content changes line wrapping.

**Current behavior:**
- Line-marker measurement runs after paint.
- A rendered Markdown update can show stale gutter offsets for one frame.
- The gutter may not realign until the RAF update or a later observer event.

**Proposal:**
- Move the initial geometry read back to `useLayoutEffect`, or synchronously clear/recompute markers while keeping observer setup separate.
- Add a regression test that line markers update immediately after a content/layout-affecting change.

## Markdown segment id stability test is too permissive

**Severity:** Low - the test can pass without proving the downstream segment stayed isolated.

The new segment-id stability test finds the downstream section with `includes("Section two original.")`. If that text is merged into a larger segment, the test can still pass while no longer proving the specific React-key stability behavior it is intended to protect.

**Current behavior:**
- The test searches for a substring instead of an exact segment shape.
- Merged segments can satisfy the assertion.
- Duplicate/repeated chunk behavior is not covered strongly enough.

**Proposal:**
- Assert the exact downstream segment shape and id.
- Add a repeated duplicate-block fixture and verify the specific downstream occurrence keeps its id across an upstream line-count shift.

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

## Diff tab scroll restore can carry over between same-mode tabs

**Severity:** Low - switching between two diff tabs that use the same view mode can leave the new tab at the previous tab's scroll offset.

`DiffPanel` resets stored scroll positions when the diff-tab identity changes, but the live scroll container restore effect is keyed to `viewMode`. If two tabs both open in the same mode, such as rendered Markdown to rendered Markdown, the effect does not necessarily apply the reset value to the newly active container.

**Current behavior:**
- Scroll-position refs are reset on diff identity change.
- The visible scroll container may not be written when `viewMode` is unchanged.
- A newly selected diff tab can start at the prior tab's offset.

**Proposal:**
- Include the diff identity, such as `diffMessageId` or `filePath`, in the restore effect.
- Or explicitly apply the reset scroll position after resetting `diffViewScrollPositionsRef`.
- Add a regression test that switches between two same-mode diff preview tabs and asserts the second tab starts at the expected offset.

## Mermaid diagram rendering hardcodes a dark theme

**Severity:** Medium - Mermaid diagrams can become illegible when TermAl runs in a light appearance because the diagram is always rendered with `theme: "dark"`.

`MermaidDiagram` in `ui/src/message-cards.tsx:704-730` calls `mermaid.initialize({ startOnLoad: false, securityLevel: "strict", theme: "dark" })` unconditionally on every render of a Mermaid block. The rest of the panel reads a `MonacoAppearance` and adapts colors, but Mermaid is hardcoded to the dark theme. In a light workspace appearance, the diagram renders with a dark background against a light page, and diagram labels can clash with the surrounding text colors.

A second concern is that `mermaid.initialize` is a global-state mutation on the imported module. Any other Mermaid consumer (now or in the future) that expects a different theme will be silently overridden the first time a `MermaidDiagram` mounts, because `mermaid.initialize` mutates a module-level singleton.

**Current behavior:**
- Every Mermaid block initializes Mermaid with `theme: "dark"`, regardless of the active appearance.
- Light appearance users see a dark diagram inside a light panel.
- `mermaid.initialize` is called inside an effect that runs on every `[code]` change, re-applying the global theme repeatedly.
- Future callers that want a different theme are silently overridden.

**Proposal:**
- Thread the current `MonacoAppearance` or a derived theme token (`"dark"` / `"light"`) from the panel into `MermaidDiagram` and pass it to `mermaid.initialize`.
- Call `mermaid.initialize` once per theme value (not once per diagram code change), or switch to `mermaid.render` options that do not mutate module-level state.
- Add a regression test that mounts a Mermaid block in light appearance and asserts the rendered SVG uses the light theme.

## Editable Markdown serialization silently drops any pasted subtree carrying `data-markdown-serialization="skip"`

**Severity:** Medium - a pasted Markdown section that contains a Mermaid diagram (or any other subtree that carries `data-markdown-serialization="skip"`) is silently excluded from the saved Markdown when the containing section is serialized.

`EditableRenderedMarkdownSection` in `ui/src/panels/DiffPanel.tsx:2213-2239` uses a plain `contentEditable` section with no `onPaste` handler. The default browser paste path preserves `data-*` attributes on HTML pastes. `shouldSkipMarkdownEditableNode` at `ui/src/panels/DiffPanel.tsx:2402-2408` unconditionally trusts the attribute:

```ts
function shouldSkipMarkdownEditableNode(node: HTMLElement) {
  return (
    node.tagName.toLowerCase() === "button" ||
    node.getAttribute("aria-hidden") === "true" ||
    node.dataset.markdownSerialization === "skip"
  );
}
```

The only writers of `data-markdown-serialization="skip"` today are `MermaidDiagram`'s rendered block and its error fallback in `ui/src/message-cards.tsx:734` / `:754`. If a user selects a Markdown section that contains a Mermaid diagram, copies it, and pastes it into another editable section (or the same section after a cancel), the pasted diagram carries the attribute forward. On the next serialize-on-blur / save / commit, the pasted subtree is silently omitted from the emitted Markdown. The user sees the Mermaid block on screen, saves, reloads, and the block is gone.

**Current behavior:**
- The editable Markdown section has no paste handler.
- Browsers preserve `data-*` attributes when HTML is pasted into a `contenteditable` region.
- `shouldSkipMarkdownEditableNode` treats attribute presence as authoritative and does not verify that the skip-marked subtree originated from a trusted renderer.
- Pasting a Mermaid diagram (or any subtree bearing the attribute) into an editable section causes it to be dropped from the serialized output on save.
- The user receives no warning and the diff view may still show the pasted diagram locally until the panel reloads.

**Proposal:**
- Add an `onPaste` handler on the editable section that sanitizes pasted HTML to strip `data-markdown-serialization` and other trust-dependent attributes, or convert the paste to plain text and re-render through the normal Markdown pipeline.
- Or stop trusting `data-markdown-serialization` alone, and also verify that the node is a known Mermaid/React-owned subtree (e.g., check for an associated React root marker or a sibling source fence).
- Add a regression test that pastes HTML containing `<div data-markdown-serialization="skip">payload</div>` into an editable section and asserts `payload` is still present in the serialized Markdown.

## Diff view scroll slots for `changes`, `edit`, and `raw` modes are effectively dead

**Severity:** Low - `createInitialDiffViewScrollPositions` allocates slots for all five `DiffViewMode` values, but three of them are never written or restored from any live scroll container.

`ui/src/panels/DiffPanel.tsx:102-110` builds `{ all: 0, changes: 0, edit: 0, markdown: 0, raw: 0 }`. `readDiffViewScrollTop` at `:315-323` only reads live DOM state for `"all"` (via `diffEditorRef.current.getScrollTop()`) and `"markdown"` (via `markdownDiffScrollRef.current.scrollTop`). For `"changes"`, `"edit"`, and `"raw"` it returns the ref slot, which is `0` on mount and never updated. `restoreDiffViewScrollTop` at `:325-344` likewise only writes to the diff editor or the Markdown scroll region. For non-diff/non-markdown modes it returns `true` without touching any DOM, so the restore effect at `:358-373` thinks the restore succeeded.

The practical effect: switching `changes` → `markdown` → `changes` does not restore the user's prior scroll position in `changes` mode; it always lands at the top. Same for `edit` (the Monaco code editor keeps its own scroll state only while the editor instance stays alive) and `raw`. Users who scroll through a long structured diff and briefly switch to Markdown preview to check something lose their place on return.

**Current behavior:**
- All five slots are allocated.
- Only `all` and `markdown` participate in read/restore.
- `changes`, `edit`, `raw` slots remain `0` forever.
- Switching away from `changes`/`edit`/`raw` and back scrolls to the top.
- The restore effect still reports success, so the bug is invisible in tracing.

**Proposal:**
- Wire up a `scrollRef` for `StructuredDiffView` and the raw patch container, and read/write through those refs in the `changes` / `raw` branches of `readDiffViewScrollTop` / `restoreDiffViewScrollTop`.
- For `edit`, read from `MonacoCodeEditor` via a new handle method (mirroring `MonacoDiffEditor.getScrollTop()` / `setScrollTop()`).
- Or, if the dead slots are intentional (known design limitation), prune them from the `DiffViewMode` slot set and add a comment.
- Add a regression test that scrolls in `changes` mode, switches to `markdown`, switches back, and asserts the prior scrollTop is restored.

## `MonacoDiffEditor.setScrollTop` writes both the original and modified editors with a single offset

**Severity:** Low - `setScrollTop(n)` pushes the same raw pixel offset into both sides of the diff editor, while `getScrollTop()` only reads the modified side. A restore that round-trips through the two methods can land the original pane off-by-lines when the two buffers have different content heights.

`ui/src/MonacoDiffEditor.tsx:80-100` declares:

```ts
getScrollTop() {
  return diffEditorRef.current?.getModifiedEditor().getScrollTop() ?? 0;
},
setScrollTop(scrollTop) {
  const editor = diffEditorRef.current;
  if (!editor) { return; }
  editor.getOriginalEditor().setScrollTop(scrollTop);
  editor.getModifiedEditor().setScrollTop(scrollTop);
},
```

Monaco's diff editor already synchronizes scroll between the two sub-editors (unchanged regions line up, hidden regions scroll together). Writing the modified side is enough for a visual restore. Also writing the same raw offset into the original side can fight Monaco's own sync and, if the original pane has a shorter/taller content height, can leave the two panes scrolled to different logical lines for a frame before the widget re-syncs.

**Current behavior:**
- `getScrollTop` samples only the modified editor.
- `setScrollTop` pushes the same offset into both editors.
- Monaco's internal scroll-sync can immediately re-sync one of them back.
- A restore that reads from modified and writes to both is asymmetric and can produce brief visual flicker or off-line alignment.

**Proposal:**
- Write only the modified editor in `setScrollTop`, and let Monaco's built-in sync handle the original pane.
- Or read from both editors in `getScrollTop` and store/restore both offsets explicitly as a pair.
- Add a regression test that sets a scroll offset, reads it back, and asserts the modified editor is the authoritative side.

## Implementation Tasks

- [ ] P2: Add lifecycle coverage for rendered Markdown drafts during file refresh/delete:
  create a test where an uncommitted contentEditable draft is active, then a file-change/reload/delete path fires, and assert the draft is flushed or the refresh is blocked.
- [ ] P2: Add workspace-layout hydration coverage for stripped diff preview document content:
  start with a persisted diff preview tab that has `gitDiffRequest` but no `documentContent`, hydrate the saved layout asynchronously, mock `fetchGitDiff`, and assert the request payload, hydrated `documentContent`, propagated `documentEnrichmentNote`, and rejection `loadError`.
- [ ] P2: Add UNC source-link normalization coverage:
  render Markdown with a `\\server\share` workspace root and a relative link, then assert the open-source target preserves the UNC root and clamps `..` at the share boundary.
- [ ] P2: Add fenced-code rendered diff coverage:
  change only an interior line of a non-Mermaid fenced code block and assert the rendered segment includes both the opening and closing fence.
- [ ] P2: Add nested unordered-list rendered diff coverage:
  replace the current false "renders the same" expectation for nested bullets,
  change `- parent\n  - child` to `- parent\n- child`, and assert the rendered
  diff records the structural list change.
- [ ] P2: Add Mermaid SVG safety coverage:
  render Mermaid inputs with malicious labels, directives, SVG event attributes,
  `javascript:` or external hrefs, and `foreignObject`, then assert the final
  output is sandboxed or sanitized before it reaches the app DOM.
- [ ] P2: Add `MermaidDiagram` behavioral coverage:
  render `MarkdownContent` with `preserveMermaidSource` toggled on/off and with
  `renderMermaidDiagrams` toggled on/off, and assert (1) the fenced source block
  is shown when `preserveMermaidSource` is true, (2) the rendered diagram `role="img"`
  container is present when `renderMermaidDiagrams` is true, (3) the
  `mermaid-diagram-loading` class is removed after `mermaid.render` resolves, and
  (4) the error fallback branch renders the source code when `showSourceOnError`
  is true and a render failure is simulated. None of these props are covered today.
- [ ] P2: Add fenced-block segmentation edge-case coverage for
  `expandChangedRangeToMarkdownFenceBlocks` / `parseOpeningMarkdownFenceLine`:
  (1) a fence opened with 4+ backticks closed only by a matching-length fence,
  (2) tilde fences (`~~~`) alongside backtick fences, (3) a fence with a
  language followed by an info string, (4) a fenced block adjacent to inline
  code and indented code, and (5) an unclosed fence at end-of-file. Each case
  should assert the segmenter treats the fence as atomic (or explicitly rejects
  it as invalid) instead of splitting opener from body.
- [ ] P2: Add a `changes`-mode scroll-isolation regression test for `DiffPanel`:
  scroll `StructuredDiffView`, switch to `markdown` mode, switch back to
  `changes`, and assert the container is restored to the prior scrollTop.
  Pairs with the dead-scroll-slots bug entry above; skip or refactor if that
  bug is resolved by pruning the dead slot.
- [ ] P2: Cover fenced-block rejection paths in `parseOpeningMarkdownFenceLine`:
  assert inline-code spans (single-backtick runs shorter than 3) do not open a
  fence, assert a fence with a non-language info string (e.g., ``` ``` with
  trailing `{title}`) still matches the fence detector, and assert a fence
  whose language token contains whitespace is parsed as language = first-word
  or rejected consistently.
- [ ] P2: Add same-mode diff tab scroll-restore coverage:
  switch between two diff preview tabs that both use rendered Markdown mode and assert the second tab does not inherit the first tab's scroll offset.
- [ ] P2: Strengthen the uncommitted downstream Markdown draft regression:
  keep the downstream contentEditable draft local while an upstream section changes line count, and assert no blur/commit occurred before the structural shift.
- [ ] P2: Fix the SourcePanel Markdown mode reset test:
  leave the panel in Preview or Split before rerendering to a non-Markdown file, then assert returning to Markdown starts in code mode.
- [ ] P2: Add response-level oversized Markdown enrichment coverage:
  exercise `load_git_diff_for_request` or the HTTP JSON path and assert raw diff, `documentEnrichmentNote`, and omitted `documentContent`.
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
- [ ] Add a code comment on the re-pin `useLayoutEffect` noting that its
  correctness depends on the arm `useLayoutEffect` (which sets
  `shouldKeepBottomAfterLayoutRef.current = true` while measuring) being
  declared BEFORE it so that React's declaration-order effect scheduling
  runs the arm first in each commit phase. Without this contract, the
  re-pin effect would miss the armed flag on the first measuring commit.
- [ ] Remove `act(...)` warnings from the rendered Markdown line-count-shift regression:
  wrap async edit/blur/save transitions with `act` or switch to `userEvent` plus explicit settled-state waits.
- [ ] Strengthen Markdown diff segment-id stability coverage:
  add exact segment-shape assertions and a repeated duplicate-block fixture for downstream id stability.
- [ ] Add a regression test for typing during `handleSave`'s await window:
  use a deferred `onSaveFile` mock, type into a rendered Markdown section after
  save starts but before the deferred resolves, resolve, and assert the later
  edit is preserved in `pendingEditValueRef` / saved on the next save. The
  existing "commits an active rendered Markdown draft before saving" test only
  covers the pre-save commit path.
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
- [ ] Add a committer-set churn perf regression:
  mount a DiffPanel with N editable Markdown sections, type a character in one
  section, and assert the committer Set delete/add count is O(1) per keystroke
  rather than O(N). Pin the finding that parent handler identity churn must
  not cascade to per-section committer re-registration.
- [ ] Cover enrichment note emission for UTF-8 / symlink-target / NOT_FOUND paths:
  extend the backend test suite with explicit cases for non-UTF-8 Markdown,
  worktree symlink target that is not a file, worktree path not found, and
  `read_git_spec_text` NOT_FOUND — each asserting `document_enrichment_note`
  is set. Pairs with the enrichment-note-coverage bug entry.
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
