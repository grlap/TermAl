# Bugs & Known Issues

This file tracks reproduced, current issues. Resolved work, speculative refactors,
cleanup notes, implementation task ledgers, and external limitations do not belong here.

## Active Repo Bugs

## Same-revision remote `TextDelta` replays can duplicate text in already-loaded proxy sessions

**Severity:** High - `src/remote_routes.rs:1083-1090`. Exact replay suppression is currently recorded only after targeted hydration succeeds. If the proxy session is already loaded, `hydrate_unloaded_remote_session_for_delta()` returns `false`; the stale-revision guard then rejects only revisions older than the remote watermark, so an exact duplicate `TextDelta` at the same remote revision can append the same text a second time.

This is a correctness issue for remote SSE duplicate delivery and reconnect replay. Same-revision sibling deltas with different payloads still need to apply, so the fix cannot collapse all same-revision events.

**Current behavior:**
- Targeted-hydration success records an exact replay key.
- Normally applied same-revision `TextDelta` events are not recorded in the replay cache.
- An exact duplicate same-revision `TextDelta` can append duplicate assistant output.

**Proposal:**
- Record exact replay keys after every successful non-idempotent session delta application, not only after targeted hydration.
- Keep the key specific enough to allow different same-revision sibling deltas.
- Add an already-loaded proxy regression that applies the same `TextDelta` twice at the same remote revision and asserts text is appended once.

## Remote hydrated-delta replay cache survives remote continuity resets

**Severity:** Medium - `src/remote_routes.rs:815-818` and `src/state.rs:55-77`. Remote applied-revision watermarks are cleared on reconnect, disconnect, or repoint paths, but `RemoteDeltaReplayCache` entries are not. A later stream epoch using the same `remote_id`, revision, and payload can be skipped by the old cache entry before the normal revision gate has a chance to rebuild continuity.

The cache is meant to suppress replays inside one remote event-stream lifetime. Keeping it across continuity resets gives old-process knowledge authority over a new stream.

**Current behavior:**
- `clear_remote_applied_revision(remote_id)` resets the remote revision watermark.
- The replay cache retains entries for the same `remote_id`.
- A same revision/payload in a new stream epoch can be dropped as an old hydrated replay.

**Proposal:**
- Clear replay-cache entries for the remote anywhere the remote applied-revision watermark is cleared.
- Or include a remote connection/server epoch in the replay key.
- Add a reconnect/repoint regression that clears the watermark, replays the same revision/payload, and asserts the event is evaluated under the new epoch.

## Targeted remote hydration trusts `messageCount` without validating loaded transcript length

**Severity:** Medium - `src/remote_routes.rs:497-512`. The targeted hydration freshness gate compares the triggering delta expectation against `SessionResponse.session.message_count`, but it does not first validate that the loaded `messages` array length matches that advertised count. A malformed or inconsistent remote response can match `messageCount` and `sessionMutationStamp` while carrying a different transcript length.

This weakens the repair gate that is supposed to prove the full response reflects the delta that triggered hydration.

**Current behavior:**
- Targeted hydration accepts a newer full response when advertised metadata matches the triggering delta.
- Loaded `session.messages.len()` is not checked against `session.message_count`.
- A count/stamp-consistent but transcript-length-inconsistent response can be adopted.

**Proposal:**
- When `messages_loaded` is true, reject `SessionResponse` values where `messages.len()` does not equal `message_count`.
- Compare delta expectations only against the validated count.
- Add a remote hydration regression with mismatched advertised count and loaded transcript length.

## Pane-level `bottom_follow` cooldown is not scoped to the scroll state key

**Severity:** Medium - `ui/src/SessionPaneView.tsx:649` and `ui/src/SessionPaneView.tsx:2552-2563`. The pane-level programmatic `bottom_follow` deadline can survive a fast session/tab switch. A scroll event in the next active session can then be treated as part of the previous session's smooth bottom-follow, force the new session to bottom, and overwrite its saved non-bottom scroll state.

This is separate from the virtualizer-side cooldown behavior: the pane-level state also needs to be keyed to the session/tab whose scroll it is managing.

**Current behavior:**
- `paneProgrammaticBottomFollowUntilRef` stores a deadline but not the owning `scrollStateKey`.
- Session/tab switches restore saved scroll while the previous bottom-follow deadline can still be active.
- A native scroll in the new session can be misclassified as programmatic.

**Proposal:**
- Store the active `scrollStateKey` with the pane bottom-follow deadline, or cancel the deadline whenever `scrollStateKey` changes.
- Add a regression that starts bottom-follow in one session, switches sessions before the cooldown expires, and asserts the second session's saved non-bottom position is preserved.

## Remote hydrated replay tests cover only `TextDelta`

**Severity:** Medium - `src/tests/remote.rs:2588`. The replay suppression coverage currently exercises the `TextDelta` path, but several `apply_remote_delta_event` arms manually participate in the hydrate/apply/note contract. A missing replay-note call in `MessageCreated`, `TextReplace`, `CommandUpdate`, or another representative delta arm could pass tests.

**Current behavior:**
- Tests pin exact hydrated replay behavior for `TextDelta`.
- Other session-mutating delta variants rely on manual cache calls without equivalent coverage.

**Proposal:**
- Add table-driven replay tests across representative delta variants.
- Or refactor the hydrate/apply/note sequence into one shared helper and test that helper's contract.

## SourcePanel rendered-Markdown mode switching lacks commit-failure coverage

**Severity:** Medium - `ui/src/panels/SourcePanel.tsx:420`. Rendered Markdown mode switching now flushes rendered drafts and aborts on commit failure, but the tests cover save, blur, and Ctrl+S paths rather than switching modes with an active draft.

If this path regresses, a user can switch editor modes with an uncommitted or rejected rendered draft and lose edits or land in the wrong mode.

**Current behavior:**
- Mode switching routes through the rendered-draft commit path.
- Tests do not cover successful mode switch after a rendered draft commit.
- Tests do not cover failed commit preserving the current mode and showing an error.

**Proposal:**
- Add successful and failed mode-switch tests.
- Assert save payload/buffer updates on success.
- Assert error banner, retained draft, and unchanged selected mode on failure.

## SourcePanel drop sanitization test does not inspect the live editable DOM

**Severity:** Medium - `ui/src/panels/SourcePanel.test.tsx:290`. The current hostile drop test verifies the saved Markdown output, but not the transient `contentEditable` DOM immediately after the drop. A sanitizer regression could briefly insert an unsafe `img`, event handler, or `javascript:` URL into the live editor and still serialize safely later.

**Current behavior:**
- Drop tests validate saved Markdown after serialization.
- Tests do not assert the live editable DOM is safe immediately after drop.

**Proposal:**
- After the drop event and before save, assert that no `img`, inline event handlers, or `javascript:` hrefs exist in the rendered editable DOM.
- Keep the saved Markdown assertion as the persistence contract.

## DiffPanel rejected rendered-draft save test does not prove retryability

**Severity:** Low - `ui/src/panels/DiffPanel.test.tsx:1022`. The rejected rendered-draft save test asserts that no save happened and that an error appeared, but it does not prove the rejected draft remained visible and retryable. The `onApplied` contract is specifically meant to preserve rejected drafts for correction or retry.

**Current behavior:**
- Test covers the error path and absence of a save call.
- Test does not assert that edited draft content remains mounted.
- Test does not retry after clearing the mocked overlap.

**Proposal:**
- Assert the rejected draft is still visible after failure.
- Clear the mocked overlap, retry the save, and assert the saved payload includes the retained draft.

## Paste/drop href validation allows UNC-style no-colon paths

**Severity:** Low - `ui/src/panels/markdown-diff-edit-pipeline.ts:250`. Paste/drop sanitization treats any href without a colon as safe. That admits Windows UNC-style or local-looking paths such as `\\\\host\\share\\file.md` and protocol-relative `//host/path` values into persisted Markdown links.

The later source-link path still performs project-scope checks, but pasted untrusted HTML should not preserve local/network filesystem-looking hrefs in the first place.

**Current behavior:**
- No-colon hrefs pass the paste/drop safety check.
- UNC-style and protocol-relative hrefs are not explicitly rejected.

**Proposal:**
- Allow anchors and document-relative paths only.
- Explicitly reject `\\`, `//`, drive-absolute paths, and rooted local paths unless they are proven workspace-scoped.

## `handleDrop` ignores drop coordinates and inserts at stale selection

**Severity:** Low - `ui/src/panels/markdown-diff-change-section.tsx:401-430`. The new `handleDrop` correctly prevents the browser default and routes payloads through `insertSanitizedMarkdownPaste`, but it ignores the drop coordinates (`event.clientX`/`clientY`). After `event.preventDefault()`, the browser does not move the caret to the drop point, and `insertSanitizedMarkdownPaste` operates on the current `window.getSelection()` (or appends to the end of the section when no selection lives inside it). The user perceives "I dropped here, content inserted somewhere else." Functionally safe — the sanitizer still runs — but UX-asymmetric.

The new SourcePanel drop test pre-calls `selectRenderedMarkdownSectionContents()` to seed a selection inside the section, so the test path always inserts where expected. Production drops without a prior selection in the section silently fall back to the end-of-section path.

**Current behavior:**
- `handleDrop` calls `insertSanitizedMarkdownPaste` without setting the selection to the drop coordinates.
- Insertion lands at the user's previous selection or appends to the end of the section.

**Proposal:**
- Use `document.caretPositionFromPoint(event.clientX, event.clientY)` (Firefox) or `document.caretRangeFromPoint(event.clientX, event.clientY)` (Chromium) to derive the drop point, set the selection to that range before calling `insertSanitizedMarkdownPaste`.
- Add a regression that drops without a pre-existing selection inside the section and asserts the content lands at the drop point, not at the end.

## `handleDrop` calls `event.preventDefault()` only after the `canEdit` check

**Severity:** Low - `ui/src/panels/markdown-diff-change-section.tsx:401-430`. The early-exit branch at line 419 returns when `(html || fallback) === ""` (an empty drop with no plain text and no URI list) without calling `event.preventDefault()`. With `canEdit && allowReadOnlyCaret === true`, the browser's default drop action then fires — typically inserting whatever payload it can extract, navigating to a URL, or dropping a file at the contentEditable cursor. The earlier `onDrop={handleReadOnlyMutationEvent}` at least called `preventDefault()` for the read-only case.

**Current behavior:**
- `handleDrop` early-returns without `preventDefault` on empty drops in editable mode.
- The browser's default drop action runs.

**Proposal:**
- Move `event.preventDefault()` above the early return, OR always call it on drop when the section is editable even if no useful payload is present.

## `text/uri-list` drop fallback lacks regression coverage

**Severity:** Low - `ui/src/panels/markdown-diff-change-section.tsx:401-430` now accepts `text/html`, `text/plain`, and `text/uri-list` payloads, but the current drop tests exercise only HTML/plain-text payloads. URI-only browser/link drops are therefore covered only by implementation intent, not by a pinned behavior test.

The path is security-sensitive and UX-sensitive because it is the fallback used when a browser exposes a dragged link as URI-list data with comments and no HTML/plain payload.

**Current behavior:**
- `text/uri-list` is parsed as a fallback in production.
- No test drops URI-list-only data with comments plus a URL.
- A regression could silently stop URI-only link drops from producing sanitized Markdown.

**Proposal:**
- Add a drop test where `text/html` and `text/plain` are empty and `text/uri-list` contains comments plus a URL.
- Assert the inserted Markdown uses the sanitized URI fallback.

## `formatSafeMarkdownLinkDestination` rejects legitimate paren-bearing URLs (silent target loss on round-trip)

**Severity:** Low - `ui/src/panels/markdown-diff-edit-pipeline.ts:380-393`. The new destination filter rejects any href containing `(`, `)`, `[`, `]`, `<`, `>`, whitespace, or control characters with no fallback. Common legitimate URLs survive paste-sanitization (`isSafePastedMarkdownHref` accepts them) but get demoted to label-only text on the next serialize round-trip — silent loss of the link target. Examples include Wikipedia disambiguation (`https://en.wikipedia.org/wiki/Foo_(bar)`) and URL fragments containing parens.

The serializer's stated contract per the file header is that links round-trip via `[text](href)`, but only a strict subset of safe URLs actually do.

**Current behavior:**
- Safe-protocol hrefs containing forbidden characters are silently rewritten to plain label text on serialize.
- Round-trip "rendered → source" is lossy for paren-bearing URLs.

**Proposal:**
- When a safe href contains forbidden characters, emit angle-bracket form `<https://example.com/Foo_(bar)>` (escaping any literal `>` inside).
- Or document the round-trip narrowing in the file header so future maintainers know the contract is "safe subset of safe URLs."
- Add a serializer test that round-trips `Foo_(bar)` through paste → DOM → serialize and asserts the destination survives.

## `formatSafeMarkdownLinkDestination` doesn't escape `\` in destinations

**Severity:** Low - `ui/src/panels/markdown-diff-edit-pipeline.ts:380-393`. Labels are escaped against `\\` via `escapeMarkdownLinkLabel` (and `[`, `]`), but destinations are not. A safe-protocol href like `https://example.com/foo\bar` round-trips as `[text](https://example.com/foo\bar)` — most CommonMark parsers treat `\b` as `b` because backslash-escape is allowed before any ASCII punctuation in destinations. The serialized destination is silently rewritten.

Defense-in-depth gap: unlikely from normal browser hrefs but the serializer's stated contract is "links round-trip", and they do not.

**Current behavior:**
- Backslash in destination passes through unescaped.
- CommonMark-compliant parsers re-interpret `\X` as `X`, rewriting the destination.

**Proposal:**
- Either reject `\` in the destination character set, or escape it during serialization.
- Add a unit test with `https://example.com/foo\bar` asserting either rejection (label-only) or proper backslash-escape on emit.

## `onDirtyChange` effect fires per parent render due to inline-callback identity

**Severity:** Low - `ui/src/panels/SourcePanel.tsx:283-285` × `ui/src/SessionPaneView.tsx:1001`. The new `useEffect(() => onDirtyChange?.(isDirty), [isDirty, onDirtyChange])` consolidates the prior six manual `onDirtyChange?.()` call sites into a single effect — a real improvement. But `handleSourceEditorDirtyChange` is declared as a fresh inline arrow on every parent render, so `onDirtyChange` identity changes per render and the effect re-fires unconditionally. The parent's `setSourceEditorDirty` bails out on no-change, so there is no infinite loop — but every parent render re-pings the parent through the dirty channel.

**Current behavior:**
- The effect's dep array includes `onDirtyChange`, an inline arrow recreated each parent render.
- Effect fires on every render even when `isDirty` is unchanged.

**Proposal:**
- Wrap `handleSourceEditorDirtyChange` in `useCallback`.
- Or, in `SourcePanel`, store the latest `onDirtyChange` in a ref and only depend on `[isDirty]`.

## Mermaid aspect-ratio trade-off for tall-narrow diagrams undocumented

**Severity:** Low - `ui/src/mermaid-render.ts:119-149`. The aspect-ratio + auto-height refactor is a clean win for wide diagrams in narrow columns (no more blank-area below the SVG). For very-tall narrow diagrams in narrow columns, the new path crops the bottom: when `max-width: 100%` reduces the iframe's width, `height: auto` reduces the iframe's used height proportionally, and the SVG inside the iframe (still rendering at its natural size) is then clipped by srcdoc's `overflow-y: hidden`. The prior path always used the unclamped frame height, so vertical content would always be visible (at the cost of blank horizontal area). Reasonable trade-off but undocumented in source.

**Current behavior:**
- `getMermaidDiagramFrameStyle` returns `{ aspectRatio, height: "auto", width, maxWidth }`.
- Tall diagrams in narrow viewports: bottom of the SVG is cropped instead of overflowing the iframe vertically.
- The trade-off is not called out anywhere in the file or in `docs/features/source-renderers.md`.

**Proposal:**
- Extend the comment block in `getMermaidDiagramFrameStyle` to call out the trade-off explicitly: "wide diagrams in narrow columns: scale to fit (no blank area); tall diagrams in narrow columns: srcdoc `overflow-y: hidden` crops the bottom rather than expanding the iframe past its aspect-ratio height."
- Optional: add a test pinning the tall-narrow path so the trade-off is enforced.

## `allowCurrentSegmentFallback` defaults to *enabled* in `RenderedMarkdownSectionCommit`

**Severity:** Low - `ui/src/panels/markdown-commit-ranges.ts:108`. The optional flag defaults via `!== false`, so the safer default for new full-document preview callers is the unsafe one. The single SourcePanel call site explicitly passes `false` (correctly), but a future caller forgetting the prop falls into the diff-pane semantics where "current segment" is taken as authoritative. The current consumer count is one external caller, so the foot-gun is narrow today.

**Current behavior:**
- `allowCurrentSegmentFallback?: boolean` defaults to enabled (`!== false`).
- Future full-document preview callers omitting the prop silently get the unsafe semantics.

**Proposal:**
- Invert the polarity: rename to `requireCurrentSegmentFallback` defaulting to `false`, OR make the field non-optional.
- Add a contract comment near the type definition explaining when each value is appropriate.

## `onApplied` clears uncommitted-but-equivalent drafts, causing contentEditable remount on round-trip

**Severity:** Low - `ui/src/panels/SourcePanel.tsx:386-391`. When `nextDocumentContentLf === sourceContent` (the user typed something that round-trips to the original after normalization), `onApplied` runs and calls `clearCommittedDraft`, which bumps `renderResetVersion`. The user's typed-but-equivalent text is replaced by the normalized base segment — DOM identity churns and any text selection is destroyed. Subtle UX echo of the previously-tracked "Per-keystroke remount in Split mode" finding, but on a narrower trigger.

**Current behavior:**
- A round-trip-equivalent edit triggers `onApplied`, which bumps `renderResetVersion` and remounts the contentEditable subtree.
- Visible text is unchanged, but DOM identity churns and selection state is lost.

**Proposal:**
- When `nextDocumentContentLf === sourceContent`, skip the `onApplied` calls (don't bump `renderResetVersion`) — just clear `hasRenderedMarkdownDraftActive`. The draft refs were already in a clean state in this branch; nothing else needs to reset.
- Add a regression that types `foo` then types backspaces back to the original, asserts the contentEditable's first child node identity is preserved across the round-trip.

## `renderedMarkdownDocumentPathRef` clear runs in `useEffect` rather than `useLayoutEffect`

**Severity:** Low - `ui/src/panels/SourcePanel.tsx:250-256`. The path-change clear runs post-paint via `useEffect`. With `<SourcePanel/>` now keyed by `tab.id ?? path` (the structural fix that landed alongside this), full remount is the primary defense and this clear-on-path-change effect is mostly redundant. But for the path-change-within-same-tab case (file rename keeping a stable tab id), the committers `Set` survives one paint after `fileState.path` changes. A `commitRenderedMarkdownDrafts()` triggered between path-change and the post-paint clear (e.g., via `flushSync(() => onCommitDrafts())` from a future `flushDraftsBeforeNavigation` handler) would still see the old committers, re-resolved against the new file's `editorValueRef.current`. Today the keying makes the window almost-always zero, so this is defense-in-depth.

**Current behavior:**
- The clear runs in `useEffect`, not `useLayoutEffect`.
- Same-tab path-change opens a single-paint window where stale committers can fire against the new file.

**Proposal:**
- Promote the clear to `useLayoutEffect` so it runs synchronously after the path change settles in render but before paint.
- Or rely entirely on the `key={...}` remount and drop the effect, since same-tab path renames are not a common code path today.

## SourcePanel async handlers can set state after unmount

**Severity:** Low - `ui/src/panels/SourcePanel.tsx` is keyed by the active source tab, but several async handlers still perform post-`await` state updates without the mounted/token guard used by `handleApplyLocalEditsToDiskVersion`. Saves, reloads, compare loads, and copy operations can resolve after the panel has unmounted or after a different source tab became active.

React will usually ignore a state update after unmount in modern builds, but the missing guard is still a lifecycle contract gap and can surface stale UI feedback if the component instance is reused before the async operation resolves.

**Current behavior:**
- `handleApplyLocalEditsToDiskVersion` has a mounted/token guard.
- `handleSave`, `handleReloadFromDisk`, `handleShowCompare`, and `handleCopyPath` do not consistently use the same pattern.
- Late async completions can write status/error/compare state after the relevant source tab is no longer current.

**Proposal:**
- Apply the same mounted/token guard pattern to every async handler that sets SourcePanel state after `await`.
- Add a test that switches source tabs while an async operation is pending and asserts stale completion does not update the new tab's UI.

## `onCut` is a no-op when `canEdit=true`, symmetric to the `onDrop` finding

**Severity:** Medium - `markdown-diff-change-section.tsx:666` wires `onCut={handleReadOnlyMutationEvent}`, which only `event.preventDefault()`s when `allowReadOnlyCaret && !canEdit`. In editable mode the browser default cut path runs. Less severe than drop (no untrusted content enters the document), but the resulting clipboard payload is rich HTML — including any sandboxed iframe markup or rendered SVG nodes — and a subsequent paste in another app or another editable surface within TermAl runs through `insertSanitizedMarkdownPaste` only on paste, not on the source. Cut/copy of a rendered Mermaid diagram carries rendered HTML rather than the source `mermaid`-fenced Markdown the user likely expects.

**Current behavior:**
- `onCut` and `onCopy` (the latter not bound) follow the browser default in editable mode.
- The clipboard payload is rich HTML, not source Markdown.

**Proposal:**
- Implement explicit `onCopy`/`onCut` handlers that write the segment's source Markdown (or a serialized substring for partial selections) instead of relying on the browser default. Set `text/plain`, `preventDefault()`, and handle the DOM removal manually.
- Group with the `onDrop` fix so cut/copy/drop are all handled symmetrically.

## `resolveWheelDeltaYPx` constants undocumented; touch input is silently not pre-warmed

**Severity:** Low - `ui/src/panels/VirtualizedConversationMessageList.tsx:173-181` (`resolveWheelDeltaYPx`) hardcodes `40` for `DOM_DELTA_LINE` and `DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT` (720) for `DOM_DELTA_PAGE` fallback with no inline comment explaining the values. Real browser/OS line-mode multipliers vary considerably (Chromium ~40, Firefox 16-20 on Linux). The constant is load-bearing for the prewarm projection — too low and trackpad inertial scrolls in LINE mode will under-grow the band; too high and a small line-mode scroll over-grows the mounted range and forces unnecessary work.

Separately, the wheel handler at `:1700-1702` is the only input modality that drives `prewarmMountedRangeForUpwardWheel`. `markUserScroll` is bound to `wheel`, `touchmove`, and `keydown`, but only the wheel handler does prewarm. Touch users (mobile/iPad) get the spacer-blank that wheel users no longer see, and touch typically produces LARGER per-gesture deltas than wheel — so the spacer-exposure window is potentially worse on touch.

**Current behavior:**
- `resolveWheelDeltaYPx` constants are bare with no doc comment.
- Only wheel events trigger prewarm; touch and keyboard do not.
- The layout-effect DOM-bounds guard at lines 1171-1230 is the implicit fallback for non-wheel inputs but its asymmetric role is undocumented.

**Proposal:**
- Add a `///`-style comment documenting `resolveWheelDeltaYPx`'s constants as deliberate over-estimates that match Chromium's typical wheel-line scroll amount; over-projection only widens the prewarmed band.
- Either (a) extend the prewarm to read `touchmove` deltas (track previous touchY, compute deltaY) and pre-warm symmetrically, or (b) document explicitly that the layout-effect DOM-bounds guard at lines 1171-1230 handles touch as a fallback.

## New `AgentSessionPanel.test.tsx` test scaffolding has hardening gaps

**Severity:** Low - `ui/src/panels/AgentSessionPanel.test.tsx` (NEW this round, +211 lines). Two small hardening gaps:

1. `:2018-2024` — `ResizeObserverMock.observe` synchronously invokes the callback during `observe(target)`. Other tests in this file use a deferred-callback shape via `resizeCallbacks` map (see line 2224 onward). A synchronous mock that schedules a `setState` mid-effect can produce a "Cannot update a component while rendering a different component" warning under certain React 18 reconciliation orderings, especially if tests are later parallelized.
2. `:2214-2215` — The `finally` block restores `Element.prototype.getBoundingClientRect = originalGetBoundingClientRect` directly. If `originalGetBoundingClientRect` is `undefined` (the type signature allows it), this would set the prototype property to `undefined`, breaking subsequent tests' `getBoundingClientRect` calls. The staged `App.scroll-behavior.test.tsx` uses `Object.defineProperty(...)` with original-descriptor checks; the same pattern would harden this test.

**Current behavior:**
- `ResizeObserverMock` invokes its callback synchronously inside `observe()`.
- `finally` restores `getBoundingClientRect` via plain assignment without descriptor check.

**Proposal:**
- Use the existing `resizeCallbacks` deferred-callback shape from the sibling tests instead of synchronously invoking the callback.
- Capture the descriptor with `Object.getOwnPropertyDescriptor` and conditionally restore (or use `Object.defineProperty` when the original was missing).

## `registerRenderedMarkdownCommitter` lacks idempotency guard

**Severity:** Low - `SourcePanel.tsx:300-305`. Adds to the live ref Set with no idempotency guard. A section that re-runs its register effect (which it does on every parent render — see "Inconsistent `useCallback` discipline" entry) will momentarily have two committer closures registered in the brief window between adding the new closure and the cleanup running. `collectRenderedMarkdownCommits` invokes BOTH; each closure inside the section calls `collectSectionEdit(section)` against the same DOM section. The first one mutates `hasUncommittedUserEditRef.current = false` and clears `draftSegmentRef.current`; the second sees the cleared state and returns `null`. Today benign, but order-sensitive — a regression here could re-introduce the previously-flagged "double-fire" parent dirty notification.

**Current behavior:**
- Set-based registration with no dedup.
- Transient duplicate registrations during effect cycles.

**Proposal:**
- Key by a stable id (e.g., the section's `useId()` value) so duplicate registrations dedupe.
- Or track a registration count and warn in dev when more than one committer is alive for the same section.

## `SourcePanel.tsx` is growing along a separable axis

**Severity:** Low - `ui/src/panels/SourcePanel.tsx` grew from ~803 to 1119 lines in this round (+316). It is approaching but has not crossed the ~2,000-line scrutiny threshold. The new responsibility (rendered-Markdown commit pipeline orchestration: collect → resolve ranges → check overlap → reduce edits → re-emit with EOL style) is meaningfully separable from the existing source-buffer/save/rebase/compare orchestration. It has its own state (`hasRenderedMarkdownDraftActive`, `renderedMarkdownCommittersRef`), pure helpers already split into `markdown-commit-ranges`/`markdown-diff-segments`, and a clean parent-callback interface.

**Current behavior:**
- SourcePanel owns two distinct orchestration responsibilities in one component.

**Proposal:**
- No action this commit. Consider extracting a `useRenderedMarkdownDrafts(fileStateRef, editorValueRef, setEditorValueState, ...)` hook in a follow-up, owning `renderedMarkdownCommittersRef`, `hasRenderedMarkdownDraftActive`, `commitRenderedMarkdownDrafts`, `handleRenderedMarkdownSectionCommits`, and `handleRenderedMarkdownSectionDraftChange`.
- The hook would expose a small surface for SourcePanel to consume and keep the file under the scrutiny threshold.

## Inconsistent `useCallback` discipline on `SourcePanel` handlers crossing the prop boundary

**Severity:** Low - `ui/src/panels/SourcePanel.tsx`. `commitRenderedMarkdownDrafts`, `commitRenderedMarkdownSectionDraft`, `handleRenderedMarkdownSectionCommits`, `handleRenderedMarkdownSectionDraftChange`, `handleRenderedMarkdownReadOnlyMutation`, `handleSelectDocumentMode`, and `handleEditorChange` are plain function declarations recreated on every render. The sibling `registerRenderedMarkdownCommitter` is correctly wrapped in `useCallback` (line 305). All cross the prop boundary into `EditableMarkdownPreviewPane` / `EditableRenderedMarkdownSection`. Combined with `normalizeMarkdownDocumentLineEndings(editorValue)` being recomputed twice per render in JSX (lines 843-870, 891-906), the editable contentEditable subtree receives shifting prop identities on every parent render.

This is the exact regression the React review checklist warns about — complex component trees with inline `components` props don't survive re-renders. `EditableRenderedMarkdownSection` does have its own internal `previousSegmentMarkdownRef`/`renderResetVersion` machinery to absorb this, but stable inputs help.

**Current behavior:**
- Inconsistent stabilization: one handler `useCallback`-wrapped, six others not.
- `normalizedEditorValue` recomputed twice per render at JSX call sites.
- Editable preview pane sees fresh prop identities on every parent render.

**Proposal:**
- Wrap the prop-crossing handlers in `useCallback` with the right deps.
- Compute `normalizedEditorValue` once at the top via `useMemo([editorValue])` and reuse in both call sites.
- Or document why identity stability is unnecessary if `EditableRenderedMarkdownSection` is robust to inline-handler thrash.

## Editable rendered Markdown surface lacks textbox semantics

**Severity:** Low - keyboard and screen-reader users can focus an editable
rendered Markdown region without an accessible control role or name.

`ui/src/panels/SourcePanel.tsx:988` labels the preview wrapper as
`aria-label="Rendered preview"`, but the actual focusable
`contentEditable` section created by `EditableRenderedMarkdownSection` does not
receive a textbox role or accessible name. Once the rendered preview becomes an
editing surface, the focus target should announce itself as editable content
rather than as an unnamed generic element.

**Current behavior:**
- The wrapper has an accessible label.
- The contentEditable child is the focusable/editable element.
- The child lacks `role="textbox"`, `aria-multiline`, and a specific label.

**Proposal:**
- Extend `EditableRenderedMarkdownSection` with an editable-only
  `aria-label` prop.
- When `canEdit` is true, set `role="textbox"` and
  `aria-multiline="true"` on the contentEditable target.
- Add a focused React Testing Library assertion for the editable rendered
  preview role/name.

## Rendered Markdown editing reuses diff-owned modules without updating ownership docs

**Severity:** Low - SourcePanel now treats `markdown-diff-*` modules as a
general rendered-Markdown editing layer, but their ownership still reads as
diff-only.

`ui/src/panels/SourcePanel.tsx` imports `EditableRenderedMarkdownSection`,
`markdown-commit-ranges`, and `markdown-diff-segments` for full-document source
editing. That reuse is practical, but the module names and comments still
present the code as diff-pane infrastructure. Future maintainers can therefore
change a "diff-only" helper without realizing SourcePanel's editable preview
depends on the same commit/range contract.

**Current behavior:**
- Diff-named modules are now consumed by SourcePanel's normal source-editing
  path.
- Ownership comments do not name SourcePanel as a supported consumer.
- The shared commit contract spans both rendered diffs and full-document
  rendered source preview.

**Proposal:**
- Either extract the shared rendered-Markdown edit contract into neutral
  modules, or update the existing module headers to document SourcePanel as a
  supported consumer.
- Add a brief SourcePanel comment near the imports naming the shared contract
  so future refactors do not accidentally break the full-document editor path.

## `bottom_follow` "enter state" reset block duplicated across two branches

**Severity:** Low - `ui/src/panels/VirtualizedConversationMessageList.tsx:1482-1495` and `:1610-1625`. Two branches duplicate the same ~six-line ref-reset (`pendingProgrammaticScrollTopRef`, `lastNativeScrollTopRef`, `shouldKeepBottomAfterLayoutRef`, `isDetachedFromBottomRef`, `hasUserScrollInteractionRef`, `lastUserScrollKindRef`, `lastUserScrollInputTimeRef`). Future ref additions risk getting added to one but not the other.

**Current behavior:**
- The `syncViewport` re-arm path and the `syncProgrammaticScrollWrite` event handler each duplicate the ref-reset cluster.

**Proposal:**
- Extract a small helper inside the effect (`function enterBottomFollowMode(node) { ... }`) and call it from both sites.

## New scroll-intent ref lacks local contract documentation

**Severity:** Low - `ui/src/panels/VirtualizedConversationMessageList.tsx:336-338` adds `pendingProgrammaticBottomFollowUntilRef` to the already-large cluster of ~30 scroll-intent / mounted-range / cooldown refs without a local doc comment. The shared scroll-sync seam now documents `bottom_follow`, but the ref itself still needs an inline reset-semantics note where future virtualizer edits will happen.

The session-virtualized-transcript subsystem is already cited in `bugs.md` history as a maintenance hazard. Each new ref or scroll-kind expands the surface that future refactors must reset / clear correctly.

**Current behavior:**
- New ref has no inline comment explaining its role or reset semantics.

**Proposal:**
- Add a one-line comment near `pendingProgrammaticBottomFollowUntilRef` explaining "Latest deadline at which a programmatic `bottom_follow` smooth-scroll can still claim native scroll ticks. Reset to `Number.NEGATIVE_INFINITY` from `markUserScroll` so user gestures cancel the cooldown."

## `bottom_follow` virtualizer state machine has no synthetic-native-scroll test coverage

**Severity:** Medium - `ui/src/panels/VirtualizedConversationMessageList.tsx:1610-1624` (production), no test. The new `bottom_follow` scroll-kind sets a 1.2s programmatic-bottom-follow window and re-classifies subsequent native scroll ticks as programmatic at lines 1467-1495. The new `App.scroll-behavior.test.tsx` asserts only that `scrollTo` is called with `top: 900, behavior: "smooth"` (the SessionPaneView side). The actual regression-prevention contract — that intermediate native scroll ticks during the smooth-scroll do NOT flip `hasUserScrollInteractionRef`, that `shouldKeepBottomAfterLayoutRef` survives, and that the cooldown re-arms each forward-progress tick — has zero direct coverage.

**Current behavior:**
- Production has the cooldown + re-classification logic in two cooperating branches (event handler + syncViewport).
- Tests only check the dispatcher side.
- The pinned prompt-send path does not assert that the dispatched programmatic scroll detail is `scrollKind: "bottom_follow"`.
- A regression dropping the `pendingProgrammaticBottomFollowUntilRef` re-arm would still pass the new test.

**Proposal:**
- Add a test that fires synthetic native `scroll` events with `scrollTop` advancing toward the bottom after a `bottom_follow` write and asserts:
  - `hasUserScrollInteractionRef` is not set (e.g., no "New response" indicator emerges on the next assistant delta).
  - `shouldKeepBottomAfterLayoutRef` survives across the smooth-scroll ticks.
  - A user-initiated wheel/keyboard event during the window cancels the programmatic-bottom-follow marker.
  - The early-exit `if (isScrollContainerNearBottom(node))` branch at 1492-1495 is exercised separately.
- Add a pinned prompt-send regression that asserts both smooth scroll and `scrollKind: "bottom_follow"` dispatch.

## Deferred-render suspension/resume producer path lacks coverage

**Severity:** Medium - `ui/src/panels/VirtualizedConversationMessageList.tsx` owns the scroll-driven deferred-render suspension path, but current tests only manually set `data-deferred-render-suspended` on `.message-stack`. They do not prove the virtualized list sets the marker during user scroll, clears it after the cooldown, or dispatches `termal:deferred-render-resume`.

This leaves the main producer path for heavy Markdown deferral unpinned even though it directly affects scroll smoothness during active sessions.

**Current behavior:**
- Tests cover consumer behavior when the suspension marker already exists.
- No test exercises scroll wiring through `suspendDeferredRenderActivation()`.
- No test asserts the resume event fires after the cooldown.

**Proposal:**
- Add an integration-style virtualized-list test with a heavy Markdown message.
- Fire a wheel/scroll gesture, assert the marker is set and heavy content stays deferred, then advance timers and assert the marker clears and `termal:deferred-render-resume` fires.

## Non-Markdown SourcePanel Preview/Split tests do not assert rendered output

**Severity:** Medium - `ui/src/panels/SourcePanel.tsx` now exposes Preview/Split modes for non-Markdown renderer sources such as `.mmd` files and doc-comment Mermaid/math regions, but the current tests primarily assert controls and chip labels. They do not click those modes and assert the detected renderer output actually appears.

This means `RendererPreviewPane`, synthetic Markdown generation, or the SourcePanel preview wiring can break while the mode availability tests still pass.

**Current behavior:**
- Tests confirm Preview/Split controls are exposed for detected renderer sources.
- Tests do not assert Mermaid/math preview output after selecting those modes.
- Renderer wiring regressions can pass as long as the controls remain visible.

**Proposal:**
- Add SourcePanel tests that open Preview and Split for a `.mmd` file or Rust doc-comment Mermaid file.
- Assert a rendered preview signal appears, such as the Mermaid frame or source-line label.

## `App.scroll-behavior.test.tsx` "scroll away from bottom" geometry never moves away from bottom

**Severity:** Medium - `ui/src/App.scroll-behavior.test.tsx:944-948`. The test writes `scrollTop = 800` with `scrollHeight = 1000` and `clientHeight = 200` — i.e., `scrollHeight - scrollTop - clientHeight = 0`. That's exactly the bottom. The "still smooth-follow even when not yet pinned" intent of the test name is undermined: the panel was already pinned, so the assertions about smooth-follow and the absence of the "New response" indicator are trivially satisfied even if `bottom_follow` did nothing.

The geometry helper duplication at `:879-899` (re-implementing `stubElementScrollGeometry` inline) compounds the issue — future definition-shape changes would need to be updated in two places.

**Current behavior:**
- Test passes regardless of whether `bottom_follow` is wired through to the virtualizer.
- Test name doesn't match the geometry it sets up.

**Proposal:**
- Change `scrollTop` to a non-bottom value (e.g., 200) and dispatch a `wheel` event with `deltaY: -300` to actually establish the "scrolled up" state, OR rename the test to match what it does.
- Extract a `stubMutableElementScrollGeometry` helper returning `{ setScrollHeight, restore }` and use it instead of the inline `Object.defineProperty` block.

## `remote_delta_replay_key` re-serializes the full `DeltaEvent` and SHA-256 hashes it on every inbound delta

**Severity:** High - `src/remote_routes.rs:443-444` × `:815-818`. The new `RemoteDeltaReplayCache` correctly suppresses exact-payload replays after a successful targeted hydration, but the cache pre-check sits at the very top of `apply_remote_delta_event` and runs for every inbound delta on every remote, even when the cache is empty. The key construction calls `serde_json::to_vec(event)` (potentially MB-sized for `MessageCreated` payloads) and then runs SHA-256 over the result — BEFORE the cheap `should_skip_remote_applied_delta_revision` check and BEFORE the variant match.

For a busy session emitting `TextDelta` chunks per token, the cost of re-serializing the message + payload + computing a 256-bit digest dwarfs the actual delta application work. Worst case a chained-remote topology multiplies this by every hop. The cache has 2048 entries; the *miss* path is paying full cost on every single delta.

**Current behavior:**
- Cache pre-check runs at the top of `apply_remote_delta_event` for every variant.
- `remote_delta_replay_key` serializes the full event payload to JSON and hashes the bytes.
- The expensive key construction runs on the common case (cache miss) for every inbound delta.

**Proposal:**
- Hash only the variant discriminant + the small set of identifying fields per variant (`session_id`, `message_id`, `message_index`, `message_count`, `session_mutation_stamp`) — these uniquely identify a delta replay just as well as the full payload.
- Or do a cheap revision-only check first and only construct the full key on revision match.
- Or only run the cache pre-check inside the per-variant arms that actually populate the cache (the six message-mutating arms in `hydrate_unloaded_remote_session_for_delta`'s success path).

## Replay-cache check fires before per-variant guards, broadening "do nothing" across all `DeltaEvent` variants

**Severity:** Medium - `src/remote_routes.rs:815-818`. The cache is fed only by `hydrate_unloaded_remote_session_for_delta` success in six message-mutating arms. But the *check* runs unconditionally at the top of `apply_remote_delta_event`, so a `SessionCreated` / `OrchestratorsUpdated` / `CodexUpdated` delta whose key happens to collide with a hydrated message delta key (astronomically unlikely under SHA-256, but possible) would be silently dropped without folding the orchestrator state. The semantic boundary of the cache is fuzzy — its name says "hydrated delta replay" but it applies to all variants.

**Current behavior:**
- Cache check is variant-agnostic.
- Six variants populate the cache; five do not.
- Cross-variant collision drops the non-hydrating variant's effects.

**Proposal:**
- Move the cache check into the per-variant arms that can hydrate (the same six arms that call `note_remote_hydrated_delta_replay`), OR scope the key to include the `match` discriminant so non-hydrating variants can never share keys with hydrating ones.
- The latter is essentially free if you adopt the High's structural-key suggestion above.

## Newer-revision targeted-repair acceptance leaves local watermark behind actual response revision

**Severity:** Medium - `src/remote_routes.rs:497-512` × `:655-657`. When the remote returns `revision=5` for a delta-driven repair targeting `min_revision=3` and metadata matches, the apply path stamps `note_remote_applied_revision(min_remote_revision)` (i.e., 3, the *triggering delta's* revision) — not `remote_response.revision=5`. The local remote applied-revision stays at 3 even though the response demonstrably reflects state at revision 5. Future deltas at revisions 4 and 5 then re-apply (hitting the replay cache as the only protection), which is fragile because the cache only protects against exact-payload replays.

**Current behavior:**
- Targeted-repair acceptance uses `min_remote_revision` for `note_remote_applied_revision`.
- The watermark stays behind the actual transcript revision the response reflects.
- Sibling/intermediate deltas at the higher revision will re-apply against an already-up-to-date transcript.
- For idempotent in-place mutations (most arms) this is benign; `MessageCreated` is not idempotent.

**Proposal:**
- Either stamp `remote_response.revision` (and rely on the replay cache for sibling protection), OR document the contract explicitly at the `note_remote_applied_revision` call site so a future maintainer doesn't get surprised by the asymmetry.
- Add a regression that lands an interleaving `MessageCreated` (or `TextDelta`) at `revision = remote_response.revision - 1` after a successful metadata-matching repair, asserting transcript correctness.

## Pane-level `handleMessageStackUserScrollIntent` doesn't cancel `bottom_follow` cooldown on scrollbar drag

**Severity:** Medium - `ui/src/SessionPaneView.tsx:2572-2574`. The new pane-level cooldown (`paneProgrammaticBottomFollowUntilRef`) is cancelled only on `onWheel`, `onTouchMove`, `onKeyDown`. A user dragging the scrollbar (mousedown on the thumb) does not produce wheel/touch/key events and gets re-pinned to the bottom by the `isPaneProgrammaticBottomFollowActive()` branch at lines 2552-2563. The user perceives "I dragged the scrollbar but it snaps back."

The virtualizer-side `markUserScroll` has the same gap (binds wheel/touchmove/keydown only at `VirtualizedConversationMessageList.tsx:1814-1816`); both should add mousedown coverage.

**Current behavior:**
- Scrollbar drag during a `bottom_follow` cooldown is silently re-classified as part of the programmatic animation.
- Mouse-only users lose scroll control during the 1.2 s window.

**Proposal:**
- Add `onMouseDown` to the message-stack wrapper that calls `handleMessageStackUserScrollIntent`, AND add `mousedown` to `markUserScroll`'s event bindings in the virtualizer.
- Add a regression that fires `mousedown` on the scroll container during a `bottom_follow` cooldown and asserts the cooldown clears.

## `RemoteDeltaReplayCache::insert` clones the key twice on insert

**Severity:** Low - `src/state.rs:62-77`. The current shape is `if !self.keys.insert(key.clone()) { return; } ... self.order.push_back(key.clone());` (paraphrased) — even on a duplicate insert path the function clones once before the membership check, and on a real insert it clones twice. The `String` `remote_id` inside the key is heap-allocated, so this churns the allocator on every per-delta insert.

**Current behavior:**
- Two heap allocations per real insert; one wasted clone on every duplicate insert.

**Proposal:**
- Restructure as `if self.keys.contains(&key) { return; } self.order.push_back(key.clone()); self.keys.insert(key);` — single clone per real insert, zero clones on duplicate.
- Or take the key by value and clone once at the boundary so callers can choose.

## `RemoteDeltaReplayCache` lacks a doc comment on its eviction contract and lifetime

**Severity:** Low - `src/state.rs:55-77`. The constant `REMOTE_HYDRATED_DELTA_REPLAY_CACHE_LIMIT = 2048` carries no rationale. The cache's process-lifetime scope (no per-remote clear on bridge shutdown / remote removal) is implicit. A future contributor reading the cache may assume eviction is correlated with remote lifecycle, or that the 2048 limit is tuned for some specific load.

**Current behavior:**
- Bounded FIFO-by-insertion eviction, capped at 2048.
- No `///` comment on the type explaining the policy or scope.
- Removed-then-re-registered remote could in theory see stale entries (it doesn't, because new revisions allocate new keys, but the invariant is implicit).

**Proposal:**
- Add a `///` doc comment on `RemoteDeltaReplayCache` explaining: bounded LRU-by-insertion replay-suppression cache; cap chosen because a hydration burst per remote rarely exceeds the band of unique inbound delta payloads observable during one connection lifetime; eviction is safe because evicted entries fall back to `should_skip_remote_applied_delta_revision`'s watermark check.

## SHA-256 over `serde_json::to_vec(event)` depends on stable byte-exact serialization

**Severity:** Low - `src/remote_routes.rs:443-451`. `serde_json` does not guarantee canonical key ordering across releases. Today, `serde_derive` emits fields in struct-declaration order deterministically — but a future field reorder in `wire.rs::DeltaEvent` would silently break replay-suppression without breaking any test. Fragile invariant.

**Current behavior:**
- The cache key depends on serde_json's byte-exact output for the event struct.
- A field reorder or `Option::is_none` skip-toggling in `wire.rs::DeltaEvent` would invalidate existing keys silently.

**Proposal:**
- Either document the dependency on `serde_derive`'s stable field ordering as a build-time invariant near `wire.rs::DeltaEvent`, OR move to a structural cache key (per the High finding above) that doesn't depend on serialization order.

## `SessionCreated` delta runs cache pre-check unnecessarily

**Severity:** Low - `src/remote_routes.rs:815-818`. The cache is never populated for `SessionCreated`, so the variant-agnostic pre-check is wasted work. Also: a future maintainer reading "this short-circuits replays" might assume `SessionCreated` is in the cache (it isn't).

**Current behavior:**
- Pre-check runs for every variant including non-hydrating ones.
- Wasted hash + lock acquisition for `SessionCreated`/`OrchestratorsUpdated`/`CodexUpdated` deltas.

**Proposal:**
- Same fix as the Medium "Replay-cache check fires before per-variant guards": move the pre-check into the hydrating variants only.

## Remote `SessionCreated` can publish a non-advancing delta

**Severity:** Medium - an unchanged inbound remote `SessionCreated` redelivery
can publish a local delta with the current revision instead of a newly committed
revision.

When `ensure_remote_proxy_session_record()` returns `changed == false`,
`apply_remote_delta_event::SessionCreated` keeps `revision = inner.revision` but
still publishes `DeltaEvent::SessionCreated`. Delta events are documented as
carrying the exact next revision. Publishing a duplicate/current revision means
clients can ignore the event as stale, and chained remote replay semantics
become ambiguous.

**Current behavior:**
- The unchanged branch does not call `commit_session_created_locked()`.
- The code still publishes `DeltaEvent::SessionCreated` with the current
  revision.
- Clients that enforce monotonic delta revisions may drop the event.

**Proposal:**
- Skip publishing when `changed == false`.
- If chained remotes require a rebroadcast, route it through an explicit
  non-delta protocol path or force a real committed revision bump with a clear
  comment explaining why.
- Add a regression for duplicate remote `SessionCreated` redelivery that asserts
  no stale/non-advancing delta is published.

## Stamp-preservation contract not honored for `SessionCreated` / `OrchestratorsUpdated.sessions[]` payloads

**Severity:** Medium - the new `docs/architecture.md` note documents that "session-scoped deltas preserve cached `session_mutation_stamp` when the wire payload omits it." The fix landed for the six message-mutating delta arms (gated with `if remote_session_mutation_stamp.is_some()`), but `SessionCreated` and `OrchestratorsUpdated.sessions[]` payloads still flow through `apply_remote_session_to_record` → `localize_remote_session(&remote_session.clone())` (`src/remote_sync.rs:312` and `:368-411`), which clobbers a cached `Some(stamp)` to `None` if the inbound payload omits it.

After such a delta lands, the next metadata-only summary's freshness gate (`remote_mutation_stamp_matches`) sees `previous == None && next == Some(_)` and demotes the proxy to `messages_loaded: false` — exactly the symptom the message-arm fix was meant to prevent. Per-delta hydration HTTP fan-out then re-issues `/api/sessions/{id}` until a stamp arrives.

**Current behavior:**
- `apply_remote_session_to_record` writes the inbound `session_mutation_stamp` directly without checking presence.
- A `SessionCreated` (or `OrchestratorsUpdated.sessions[]`) payload omitting the stamp wipes a cached `Some(_)` value.
- The `docs/architecture.md` doc note implies this path also preserves cached stamps; the implementation diverges.

**Proposal:**
- Gate the stamp assignment in `apply_remote_session_to_record` on `remote_session.session_mutation_stamp.is_some()` (with the existing `previous_remote_mutation_stamp` already captured at line 311).
- Or tighten the doc note to say "narrow message-mutating deltas" and explicitly call out that `SessionCreated`/`OrchestratorsUpdated.sessions[]` payloads are authoritative session shapes whose absent stamps replace the cached value.
- Add a regression where a `SessionCreated` summary with `session_mutation_stamp: None` lands against a cached `Some(_)`; assert the cached stamp survives.

## Repeated `commit_locked` boilerplate at four early-exit paths in `hydrate_remote_session_target`

**Severity:** Low - `src/remote_routes.rs:561-606`. The new `remote_state_applied` flag must be checked and `commit_locked` called before each of four early-exit paths. The same `if remote_state_applied { commit_locked(...).map_err(...)?; } return Err(...)` boilerplate is duplicated. A future maintainer adding a fifth early-exit could easily forget the conditional commit, reintroducing the watermark-race bug under a new condition.

**Current behavior:**
- The conditional commit-before-error is replicated four times in close proximity.
- No helper or invariant comment near the function body documents that every error-return must commit broad state when applied.

**Proposal:**
- Extract a small helper closure (`bail`) that takes an error and conditionally commits before returning.
- Or invert the flow so the broad-state apply runs after the can-this-target-survive checks, eliminating the need to commit-before-error.

## `remote_session_metadata_matches_record` is permissive on both-None stamps

**Severity:** Low - `src/remote_routes.rs:428-431`. Returns `true` when both stamps are `None` — only `message_count` distinguishes a count-equal stale transcript from a current one. Consistent with sibling helpers but a remote that never emits stamps has no positive freshness evidence; a count-equal stale transcript can still be applied through the matching-metadata pass-through branch.

**Current behavior:**
- Both-None stamps yield a `true` match by virtue of `message_count` alone.
- The function has no doc comment explaining the both-None acceptance.

**Proposal:**
- If positive-stamp evidence is required for the older-revision branch, change to `record.session.session_mutation_stamp.zip(session.session_mutation_stamp).is_some_and(|(a, b)| a == b) && session_message_count(record) == session.message_count`.
- Or document the both-None acceptance in the helper's doc comment so the policy is explicit.

## GET-path 502 error message after broad-state commit is misleading

**Severity:** Low - `src/remote_routes.rs:557-572`. When `current < response.revision` triggers the rejection, the broad state has already been committed (line 561-565 `if remote_state_applied { commit_locked(...) }`) and `current_remote_revision` may now match or exceed the response revision. The error message reads "newer than synchronized remote state" — diagnostically confusing post-commit because the synchronized state was just advanced.

**Current behavior:**
- Error message refers to "synchronized remote state" without acknowledging that the commit-before-error path has already advanced it.

**Proposal:**
- Refine the message to "remote session response revision {} cannot be safely applied; broad state advanced to revision {} but transcript may have changed".
- Or split the check so the broad-state apply happens only after rejection-eligibility is confirmed.

## `remote_mutation_stamp_matches` demotes on either-side `None`

**Severity:** Medium - `src/remote_sync.rs:316-319` uses `Option::zip(...).is_some_and(...)`, which evaluates to `false` whenever either side is `None`. Combined with the just-listed stamp-clobber behavior, any remote that does not emit `session_mutation_stamp` will demote loaded proxy transcripts to `messages_loaded: false` on every metadata-only summary. The per-delta hydration HTTP fan-out then re-issues `/api/sessions/{id}` for every subsequent delta on that proxy until a stamp arrives.

The contract isn't documented at the call site — a future reader cannot tell whether `None` means "unknown freshness, be safe" or "no mutation observed."

**Current behavior:**
- `Option::zip(None, _) == None`, so any missing stamp on either side fails the match.
- A Phase-1-compatible upstream (older builds without stamps) demotes loaded transcripts on every metadata refresh.

**Proposal:**
- Add a `///` doc comment near `apply_remote_session_to_record` describing the four `(prev, next)` stamp-presence cases and the chosen demotion policy.
- If a Phase-1-compat mode is desired (preserve when both are `None`), make it explicit via `previous_stamp == next_stamp || (previous_stamp.is_none() && next_stamp.is_none())` and document the rationale.
- Add a regression for the both-None case.

## GET-path resync broadens the route's side-effect surface from "one session" to "all proxy records for the remote"

**Severity:** Medium - `src/remote_routes.rs:470-525` calls `apply_remote_state_if_newer_locked(..., None)` with `focus_remote_session_id: None`. The broad sync runs `retain_sessions`/upsert across every proxy record for that remote, so a single `GET /api/sessions/{id}` for one proxy can mutate all proxy records on the remote. `docs/architecture.md:183` still describes the route as "Fetch one session (full transcript hydration)" with no mention of the global-resync side-effect or the additional `/api/state` outbound call.

**Current behavior:**
- The route's documented contract is "fetch one session"; the actual implementation also performs a per-remote state sync as a side-effect.
- Frontend authors auditing latency expectations have no signal in the doc.

**Proposal:**
- Pass `focus_remote_session_id: Some(&target.remote_session_id)` to scope the sweep to the targeted session, OR explicitly document the broad sweep with a comment near line 506.
- Update `docs/architecture.md` to reflect the new latency contract: targeted hydration may issue an additional `/api/state` fetch and a global SSE broadcast.

## GET-path skipped-side-fetch branch does not advance the per-session watermark

**Severity:** Medium - `src/remote_routes.rs:559-564` advances `note_remote_applied_revision` only when the side-fetch ran and `apply_remote_state_if_newer_locked` accepted it. If `latest_remote_revision >= remote_response.revision` at the pre-lock check, the side-fetch is skipped and the watermark is never explicitly bumped for `remote_response.revision`. This works today because the watermark already covers the response revision, but the implicit invariant ("if we skipped because we're caught up, we don't need to advance") is undocumented and a future refactor could break it.

**Current behavior:**
- Three branches diverge in whether the watermark is advanced: side-fetch ran and applied / side-fetch ran and was rejected / side-fetch skipped.
- The skipped branch relies on `latest_remote_revision >= remote_response.revision` already holding.

**Proposal:**
- Unconditionally call `inner.note_remote_applied_revision(&target.remote.id, remote_response.revision)` after applying the targeted response. The op is idempotent (`max`-based) so it is safe under all three branches and removes the implicit invariant.
- Move the `note_remote_applied_revision` call above the `commit_locked` invocation so the published SSE state event reflects an already-advanced watermark (consistent with `apply_remote_delta_event`'s ordering).

## New stale-skip / equality-rejection tests use unrealistic fixtures

**Severity:** Low - `src/tests/remote.rs:2475,2580` construct `MessageCreated` deltas with `session_mutation_stamp: None`. Real backend emitters always emit `Some(record.mutation_stamp)`. The unrealistic fixture masks the stamp-clobber issue (the delta arms unconditionally write `None` over cached `Some(_)`).

**Current behavior:**
- New tests use `session_mutation_stamp: None` instead of the production wire shape.
- The stamp-clobber bug is not surfaced because the cache is also `None` in the fixture.

**Proposal:**
- Change the fixtures to `session_mutation_stamp: Some(...)` matching the production wire shape.
- Add a separate dedicated regression for the `None`-clobber case once that bug is fixed.

## `adoptFetchedSession` clobbers in-flight deltas read from a stale `sessionsRef` snapshot

**Severity:** High - `ui/src/app-live-state.ts:1099-1172` reads `previousSessions = sessionsRef.current` at the top of the function, computes `existingIndex` and `currentSession = previousSessions[existingIndex]`, runs the hydration-still-matches predicates against that stale snapshot, then does `sessionsRef.current = nextSessions; setSessions(nextSessions)` where `nextSessions = previousSessions.map(...)`. Between the snapshot read and the eventual write, same-instance SSE delta paths (e.g., `messageCreated`, `textDelta`) may have already mutated `sessionsRef.current` in place. The captured `messageCount` / `sessionMutationStamp` guard catches *count*-changing deltas but not in-place text mutations on the latest assistant message.

The Phase 2 hydration test scenarios pass because they add a delta that bumps `messageCount`. A same-`messageCount` shape mismatch — e.g., a `textDelta` that only changes content of the same message — is uncovered. The result on a live system would be a hydration response landing after a streaming chunk and silently rewinding the visible text to the snapshot.

**Current behavior:**
- `adoptFetchedSession` snapshots `sessionsRef.current` once at the top of an async-resolved path.
- After the awaited `fetchSession()` resolves, the function writes back via `previousSessions.map(...)`, discarding any same-instance deltas that landed during the in-flight fetch.
- The `hydrationRequestStillMatchesSession` predicate runs against the same stale snapshot, so the gate cannot detect in-place mutations.

**Proposal:**
- Re-read `sessionsRef.current` immediately before constructing `nextSessions` and re-derive `existingIndex` / `currentSession` against that fresh snapshot.
- Re-run `hydrationResponseMatchesSession` on the fresh snapshot before writing.
- Add a regression that lands a `textDelta` that does not change `messageCount` while a hydration is in flight, and asserts the streamed text survives.

## `get_session()` for unloaded remote proxies silently changed its latency contract

**Severity:** Medium - `src/state_accessors.rs:151-181` quietly turned `GET /api/sessions/{id}` from a constant-time local read into a synchronous outbound HTTP fetch + global SSE state broadcast (via `commit_locked`) when the proxy is unloaded. A slow or wedged remote stalls every visible-pane hydration request and every reconnect resync; a remote that returns `messages_loaded: false` returns `bad_gateway` to the local client instead of degrading to the unloaded summary. The new contract is not documented in `docs/architecture.md`, which still describes `GET /api/sessions/{id}` as the cheap full-hydration route.

(See the sibling entry above, "GET-path resync broadens the route's side-effect surface", for the additional global-sweep side-effect.)

**Current behavior:**
- `get_session()` performs synchronous remote HTTP I/O in the unloaded-proxy branch.
- The hydration call publishes a global SSE state event via `commit_locked`, fanning out to every connected client.
- `bad_gateway` propagates to the caller when the remote returns metadata-only or refuses; no fallback to the local unloaded summary.

**Proposal:**
- Document the new latency dependency in `docs/architecture.md` next to the "GET /api/sessions/{id} stays as the full hydration route" note, including the remote round-trip and SSE broadcast.
- Add a remote-fetch timeout that falls back to returning the local unloaded summary (`messagesLoaded: false`, `messageCount` from cache) instead of bubbling 502 to the browser.

## `hydrate_remote_session_target` rejects upstream `messages_loaded: false`, breaking chained-remote topology

**Severity:** Medium - `src/remote_routes.rs:444-462,558-561` returns `ApiError::bad_gateway("remote session response did not include a full transcript")` when an upstream remote responds with `messages_loaded: false`. On the delta path (`min_remote_revision.is_some()`), this propagates back through `hydrate_unloaded_remote_session_for_delta` as a fatal `anyhow::Error`, converting every same-session inbound delta into a hard error. In a chained-remote topology where the immediate upstream is itself proxying a third remote with an unloaded record, every session-scoped remote delta now becomes a hard error and triggers a fallback resync loop until the inner chain repairs itself.

`hydrate_unloaded_remote_session_for_delta` also collapses the structured `ApiError` (with status code) into a flat `anyhow!("failed to hydrate remote session ...: {err.message}")`, discarding the `bad_gateway` vs `not_found` distinction that downstream recovery branches might need. This is a soft observability/extensibility cost on top of the chained-remote correctness issue.

**Current behavior:**
- `hydrate_remote_session_target` enforces strict `messages_loaded: true` regardless of caller context.
- The wrapped `anyhow::Error` flows back through `apply_remote_delta_event` and triggers state resync.
- `ApiError` status codes are lost before downstream consumers see them.

**Proposal:**
- Gate the strict `messages_loaded` rejection on `min_remote_revision.is_none()` so the GET path keeps strict-mode but the delta-fast-path tolerates the chained-summary case.
- Return `Result<bool, ApiError>` from the helper (or wrap with `anyhow::Error::context(...)`) so the original error category is preserved.
- Add a regression with a fake remote that returns `messages_loaded: false` to confirm the delta path falls through to normal apply rather than hard-erroring.

## Per-delta hydration HTTP fan-out has no in-flight deduplication

**Severity:** Medium - `src/remote_routes.rs:505-534` adds `hydrate_unloaded_remote_session_for_delta` calls at the top of eight delta handlers (`MessageCreated`, `MessageUpdated`, `TextDelta`, `ThinkingDelta`, `CommandUpdate`, `ParallelAgentsUpdate`, plus two more). For a burst of N inbound deltas on a still-unloaded proxy, each call drops the lock, performs a synchronous HTTP fetch, and reacquires the lock — without any in-flight tracking. The first fetch flips `messages_loaded: true` and subsequent fetches short-circuit, but the in-flight ones still serialize on the remote registry and on the local async runtime.

A 100-delta burst on an unloaded proxy issues up to 100 HTTP fetches in sequence before the per-delta short-circuit kicks in. On chained-remote topologies where many proxies are unloaded after a summary `state` arrives, a small flurry of inbound activity can wedge the remote registry queue.

**Current behavior:**
- Eight delta handlers call `hydrate_unloaded_remote_session_for_delta` without coordination.
- Each call independently sees `messages_loaded: false`, drops the lock, fetches, and reacquires.
- The first fetch wins; subsequent fetches still serialize.

**Proposal:**
- Track in-flight hydrations per `(remote_id, remote_session_id)` (e.g., `HashMap<_, Arc<Notify>>` or a per-session `AtomicBool` + waiter pattern).
- Have parallel callers `await` the same future, falling through to the existing skip path on the first success.
- Add a regression with concurrent same-session `MessageCreated` deltas that asserts only one HTTP fetch is issued.

## `adoptFetchedSession` allow-downgrade flag duplicates the server-restart guard at two layers

**Severity:** Medium - `ui/src/app-live-state.ts:1146-1156` ORs `isServerRestartResponse` with `canAdoptLowerRevisionHydration` to compute `allowRevisionDowngrade`, but `shouldAdoptSnapshotRevision` already short-circuits restart cases via the `isServerInstanceMismatch` branch. The dead-flag interaction means restart vs. downgrade semantics are encoded across two layers of branching that must stay aligned — a regression that narrows `shouldAdoptSnapshotRevision`'s instance-mismatch branch would silently start using the downgrade path for restart cases, with subtle semantic differences.

This gate is the only frontend defense against the prior High-severity stale-hydration data loss. Two near-redundant guards encoding the same intent are fragile.

**Current behavior:**
- Restart-vs-downgrade decisions are computed at two layers (`adoptFetchedSession` and `shouldAdoptSnapshotRevision`).
- A regression in either layer can silently shift behavior without surfacing in tests.

**Proposal:**
- Split into three explicit branches: `if (isServerRestartResponse) { adopt unconditionally } else if (canAdoptLowerRevisionHydration) { adopt with downgrade } else { gate via shouldAdoptSnapshotRevision }`.
- Same observable behavior, single layer of branching, easier to audit.

## `scheduleHydrationRetry` re-runs the entire hydration effect on every tick

**Severity:** Medium - `ui/src/app-live-state.ts:647-670` arms a `setTimeout` whose callback fires `setHydrationRetryTick(...)`, a `useState` counter included in the visible-session hydration effect's deps. Each tick re-runs the whole effect and walks every visible session in `sessionIdsToHydrate`, even if only one session was retrying. With multiple sessions in pending-retry state, the cascading effect re-runs multiply network requests and CPU work under load.

The early-out at the top of the effect (`hydratingSessionIdsRef.current.has(sessionId)`) only guards against parallel in-flight requests, not against re-evaluation of unrelated sessions.

**Current behavior:**
- `setHydrationRetryTick` is a global counter dep.
- A retry for session A bumps the tick and re-evaluates session B's hydration too.
- The retry timer cleanup lives in a separate `useEffect(() => () => cancelHydrationRetries(), [])` rather than the same effect.

**Proposal:**
- Replace the tick counter with a per-session `Set<string>` ref of pending-retry ids; the timer adds the id and triggers a targeted re-fetch through a stable `useCallback` rather than re-running the effect.
- Or have the retry timer call into the fetch loop directly, bypassing the effect entirely.
- Skip work in the effect for sessions not in the retry set.

## `apply_remote_delta_event::SessionCreated` clones the full transcript only to read `.id`

**Severity:** Medium - `src/remote_routes.rs:681-690` builds `local_session = wire_session_from_record(local_record)` (a full transcript-bearing `Session`) just to consume `.id`. Both `delta_session.id` and the in-scope `local_session_id` already hold the same value. For a long-lived remote session whose proxy mirror already has a non-trivial transcript, every inbound `SessionCreated` redelivery rebuilds and drops the full message vec under the state mutex.

This is the same clone-and-discard anti-pattern the rest of this changeset migrated away from for `wire_session_summary_from_session`. The publish path here only reads the id.

**Current behavior:**
- `wire_session_from_record(local_record)` runs once per `SessionCreated` redelivery, just to access `.id`.
- The cloned `Session` is then dropped without further use.

**Proposal:**
- Drop the `local_session` binding entirely; publish using `delta_session.id.clone()` or the already-in-scope `local_session_id`.

## `commit_session_created_locked` summary fallback diverges between branches

**Severity:** Medium - `src/codex_thread_actions.rs:165-180` and `src/session_crud.rs:296-312` use `inner.find_session_index(...).map(|index| ...).unwrap_or_else(|| ...)` to build the published delta. The success arm reads from the index lookup; the fallback arm builds off the caller's `&record` parameter. The two paths can disagree on `messageCount` if the field cache (`record.session.message_count`) and the just-pushed `messages.len()` ever drift. The freshly pushed record cannot legitimately be missing from the index, but a silent divergence between the success and fallback arms is a contract wedge that can hide future regressions.

**Current behavior:**
- The fallback arm's existence implies the index lookup might fail.
- The two arms compute summaries from different sources without explicit equivalence.

**Proposal:**
- Drop the `unwrap_or_else` fallback and replace with `.expect("just-created session must be present in the index")`.
- Or route both arms through a single `wire_session_summary_from_record(record)` helper sourced from the actual `&SessionRecord` passed to `commit_session_created_locked`.

## `installMonacoCancellationRejectionFilter()` default arg crashes in non-DOM contexts

**Severity:** Low - `ui/src/monaco-cancellation-filter.ts:8` defaults `target: RejectionTarget = window`. In a non-DOM context (e.g., SSR rendering, a Node-side test harness, or a future Vitest entry that doesn't load `jsdom`), the default-arg evaluation `ReferenceError`s before any guard inside the function body runs.

The internal `typeof target.addEventListener !== "function"` guard catches *passed-in* non-DOM-like targets, but cannot help when the *default* expression itself fails.

**Current behavior:**
- `target: RejectionTarget = window` is unguarded.
- A future SSR or non-jsdom test entry calling `installMonacoCancellationRejectionFilter()` (no args) crashes at module-load time.

**Proposal:**
- `target: RejectionTarget = typeof window !== "undefined" ? window : ({} as RejectionTarget)`.
- The internal guard then no-ops cleanly when no real `window` is available.

## `captureHydrationRequestContext` null fallback defeats the gate

**Severity:** Low - `ui/src/app-live-state.ts:1019-1033` falls back to `null` for both `messageCount` and `sessionMutationStamp` when the captured summary lacks them. The match predicates (`hydrationRequestStillMatchesSession`, `hydrationResponseMatchesSession`) accept any value when the captured field is `null`, so a captured-context with both fields `null` matches any response — exactly the mixed-instance restart window when the gate matters most.

The wire contract per `Session.messageCount?: number | null` says the field is optional in payloads. After Phase 2 the backend should always emit it, but during the cross-version window the gate becomes a no-op.

**Current behavior:**
- `null` means "accept any response value" rather than "must match exactly null."
- The capture-at-send / reject-at-receive gate is silently bypassed when the captured context lacks metadata.

**Proposal:**
- Treat `null` as a "must match exactly null" marker — a numeric response value should reject when the capture was null.
- Or short-circuit to a recovery resync (`requestActionRecoveryResyncRef.current()`) when the captured context has `null` for both `messageCount` and `sessionMutationStamp`.

## No-change branch builds an unused summary in remote codex/create proxies

**Severity:** Low - `src/remote_codex_proxies.rs:94-95` and `src/remote_create_proxies.rs:183-184` build `wire_session_from_record(&local_record)` and `wire_session_summary_from_record(&local_record)` even when the surrounding `announce_remote_session_created_if_changed` short-circuits because nothing changed. The summary is then dropped on the no-change branch, wasting a metadata-shape clone under the state mutex.

The cost is small (no transcript clone — the helper now sources fields from the record directly), but the no-change branch is hot for clients re-creating an already-mirrored remote session.

**Current behavior:**
- Both helpers run unconditionally.
- The `delta_session` summary is dropped on the no-change path.

**Proposal:**
- Build the summary lazily inside the `if changed { ... }` arm, or pass an `Option<Session>` to the announce helper that defers construction until the announce path actually publishes.

## `docs/architecture.md` missing `OrchestratorsUpdated.sessions[]` skip-when-empty contract

**Severity:** Low - `docs/architecture.md:258-259` documents `OrchestratorsUpdated { revision, orchestrators[], sessions[] }` as a metadata-first delta but does not mention that `sessions[]` carries `#[serde(default, skip_serializing_if = "Vec::is_empty")]` (`src/wire.rs:1346-1347`). A reader auditing the wire contract cannot tell from the doc whether the field is required, optional, or omitted-when-empty.

**Current behavior:**
- The doc's `OrchestratorsUpdated` entry omits the empty-elision behavior.
- Readers must consult `src/wire.rs` to confirm the contract.

**Proposal:**
- Append "`sessions[]` is omitted on the wire when empty (`#[serde(skip_serializing_if = "Vec::is_empty")]`)" to the doc note.
- Confirm `ui/src/types.ts:638` (`sessions?: Session[]`) is consistent with the elision contract.

## Interaction request routes can hide same-revision `MessageUpdated` deltas

**Severity:** Medium - route responses can advance the client past the same-revision SSE delta that contains the actual card replacement.

The interaction submission routes publish `MessageUpdated` deltas, but the
initiating client also adopts the POST response. If that response is
metadata-only and carries the same revision, the client can ignore the matching
SSE delta as already seen, leaving approval, user-input, MCP elicitation, or
Codex app-request cards stale until another hydration path repairs them.

**Current behavior:**
- `src/codex_submissions.rs` interaction routes publish a `MessageUpdated`
  delta for the resolved interaction card.
- The route response can be adopted by the frontend before the same-revision
  SSE delta is processed.
- The response does not necessarily include the fully mutated session content
  needed to make skipping the delta safe.

**Proposal:**
- Return a snapshot with the mutated session fully hydrated, matching the
  `send_message` response shape used for the active session.
- Or have the frontend apply the returned message update directly without
  advancing past the same-revision SSE delta.
- Add a regression where the POST response arrives before the SSE delta and the
  resolved card is still visible immediately.

## Queued-turn dispatch publishes stale `sessionMutationStamp`

**Severity:** Medium - the `MessageCreated` delta for a queued turn can carry a mutation stamp from before pending-prompt cleanup.

Queued-turn dispatch builds the started-turn delta before all queued-prompt
state has been popped and synced. Those later mutations restamp the session
record, so the published delta can advertise a stale `sessionMutationStamp`.
The next metadata snapshot may then look like a new unseen mutation and trigger
unnecessary transcript invalidation or hydration.

**Current behavior:**
- `src/turn_dispatch.rs` captures the delta stamp before the queued prompt list
  reaches its final state for the dispatch.
- The session record can be restamped by pending-prompt cleanup after the delta
  payload was built.
- Metadata-first reconciliation relies on those stamps to decide whether a
  cached transcript is still complete.

**Proposal:**
- Build or refresh the `StartedTurnMessageDelta` after all queued-prompt
  mutations are complete.
- Assert in tests that the published delta stamp equals the final session
  record's `mutation_stamp`.

## Live-delta recovery test no longer proves the recovered message is visible

**Severity:** Medium - a watchdog regression test can pass even if the latest recovered assistant message is hidden.

One App-level live-delta recovery test no longer asserts that the recovered text
is actually rendered after the delta flush and after the stale fetch is
rejected. That weakens coverage for the same class of bug where the newest
assistant message only appears after another prompt, focus change, or unrelated
rerender.

**Current behavior:**
- `ui/src/App.live-state.watchdog.test.tsx` still exercises the recovery flow.
- The test does not prove `"Recovered from live delta."` is visible at the
  critical points.

**Proposal:**
- Restore non-brittle visibility assertions after the delta is applied and
  after the stale fetch resolves.
- Prefer `screen.getAllByText(...).length > 0` when duplicate text can appear in
  both message content and previews.

## Backend streaming delta wire tests do not pin `messageCount`

**Severity:** Medium - four backend delta emitters can drop `message_count` without failing Rust wire-contract tests.

`messageCount` now drives metadata-first reconciliation and hydration decisions,
but backend tests currently pin the field strongly for `MessageCreated` /
`MessageUpdated` paths and not for every streaming delta variant. Frontend
reducer tests help, but they cannot catch a backend serialization omission on a
specific emitter path.

**Current behavior:**
- Local backend coverage does not assert `message_count` for representative
  `TextDelta`, `TextReplace`, `CommandUpdate`, and `ParallelAgentsUpdate`
  broadcasts.
- A future Rust-side omission could still produce current-looking frontend unit
  test fixtures.

**Proposal:**
- Add backend tests that subscribe to delta events and drive one representative
  local mutation for each missing delta type.
- Assert the emitted `message_count` matches the transcript length or expected
  metadata count for that mutation.

## Metadata-first summaries make transcript search incomplete

**Severity:** Medium - search can silently miss transcript matches for sessions that have only metadata summaries loaded.

`/api/state` now returns session summaries with `messages: []` and
`messagesLoaded: false`. The session search index still walks
`session.messages` directly, so non-visible sessions can be treated as having
no searchable transcript even though the transcript simply has not been
hydrated in this browser view.

**Current behavior:**
- `ui/src/session-find.ts` builds transcript search items from
  `session.messages`.
- Metadata-first session summaries clear `messages` before reaching the
  frontend.
- Search has no "transcript not loaded" state and no on-demand hydration path
  before concluding that there are no message matches.

**Proposal:**
- Gate transcript search to hydrated sessions and surface incomplete results
  when a session summary is not loaded.
- Or hydrate/index target sessions on demand when search needs transcript
  content.
- Add coverage proving metadata-only summaries do not silently produce false
  "no transcript match" results.

## Metadata-first state summaries still broadcast full pending prompts

**Severity:** Low - transcript payloads were removed from global state, but queued prompt text can still ride along with every session summary.

Metadata-first state summaries clear `messages`, but the session summary still
includes full pending-prompt data. Queued prompts can contain user-authored
instructions or expanded prompt content, so this remains a smaller but real
data-minimization leak in `/api/state` and SSE `state` broadcasts.

**Current behavior:**
- `src/state_accessors.rs` builds transcript-free summaries but keeps the full
  `pending_prompts` projection.
- Every listening tab can receive pending prompt content for sessions it is not
  actively hydrating.

**Proposal:**
- Project pending prompts to a bounded metadata-only summary in `StateResponse`.
- Keep full queued-prompt content on targeted full-session responses where the
  active pane actually needs it.

## App-level delta fixtures omit required `messageCount`

**Severity:** Low - some tests still dispatch impossible delta payloads after the protocol made `messageCount` required.

Several App-level delta tests construct `delta` events by hand and omit
`messageCount`. Those fixtures no longer match the current `DeltaEvent` wire
contract, so they can pass through behavior that production SSE cannot produce
and miss metadata-first regressions.

**Current behavior:**
- Some `ui/src/App.live-state.deltas.test.tsx` fixtures dispatch current
  protocol delta types without `messageCount`.
- The tests are not forced through a typed helper that requires the full
  current event shape.

**Proposal:**
- Introduce a typed test helper for `DeltaEvent` fixtures and require
  `messageCount` on all session-scoped deltas.
- Update hand-written fixtures to match the current SSE contract.

## Hydration retry loop can spam persistent failures

**Severity:** Low - visible-session hydration retries clamp to the last retry delay and can continue indefinitely for persistent non-404 failures.

The new retry loop correctly recovers from stale hydration rejection and transient `fetchSession` failures, but it has no ceiling. A visible metadata-only session whose targeted hydration keeps failing will retry every 3 seconds and repeatedly call the normal request-error reporting path.

**Current behavior:**
- `ui/src/app-live-state.ts` schedules retry delays of 50 ms, 250 ms, 1000 ms, then 3000 ms, and clamps all later retries to 3000 ms.
- Non-404 `fetchSession` failures report the request error and schedule another retry.
- The transient non-404 failure branch is not covered by a regression test.

**Proposal:**
- Cap repeated user-facing error reporting or retry attempts for the same visible session while keeping event-driven or manual recovery possible.
- Add a test where the first `/api/sessions/{id}` request fails with a non-404 error, the retry succeeds, and the transcript appears without a tab switch or unrelated state event.

## Remote test module size slows review and triage

**Severity:** Note - `src/tests/remote.rs` is large enough that focused remote
review now has to scan many unrelated scenarios.

The file contains hydration, delta, orchestrator, proxy, and sync-gap coverage
in one module. New hydration/replay tests are coherent, but keeping every remote
scenario in the same file makes future review targeting and regression triage
harder, especially as the metadata-first remote work continues adding focused
cases.

**Current behavior:**
- Remote tests for several boundaries live in one oversized module.
- New review findings repeatedly point into the same large file, making
  ownership and intended fixture reuse harder to see.

**Proposal:**
- Split remote tests by boundary, for example `remote_hydration.rs`,
  `remote_deltas.rs`, and `remote_orchestrators.rs`.
- Move shared fake-server and remote-session helpers into a small support
  module used by those test files.

## New orchestrator summary-preservation test missing `.all()` shape assertion

**Severity:** Medium - `src/tests/remote.rs:1505-1521` adds a test covering `OrchestratorsUpdated.sessions` summary preservation after a full `sessions` snapshot, but the assertion only `.find()`s one session by its `message_count == 2` and checks its individual fields. It does not assert that the `.all()` of the projected sessions match the summary-shape invariant (`messages == []`, `messages_loaded == false` when expected, all `message_count` values preserved from the incoming snapshot).

A regression that silently left one session with a full transcript, or that swapped `messages_loaded` on an unrelated session in the batch, would pass the current `.find(|s| s.message_count == 2)` assertion. The test is meant to pin the "the whole republish is metadata-first" contract but only inspects one session.

**Current behavior:**
- Test finds one session by `message_count == 2` and asserts its shape.
- No `.all()` assertion over the full snapshot's `sessions` vec.
- No fixture coverage for multi-session snapshots with a mix of hydrated/unhydrated incoming records.

**Proposal:**
- Replace the `.find()` probe with `assert!(republished.iter().all(|s| s.messages.is_empty() && !s.messages_loaded))`, or add it alongside the existing assertion.
- Expand the fixture to include at least two sessions with distinct `message_count` values so the `.all()` assertion covers more than one session shape.
- Optional: parameterize over a hydrated-input + unhydrated-input mix to also pin the "republish projects metadata regardless of source hydration state" contract.

## Reconnect fallback test no longer proves applied delta is visible

**Severity:** Low - reconnect coverage can pass even if the session delta in the scenario is ignored.

One backend connection test still describes an applied session delta, but the
assertion that proved the delta changed the visible session state was removed.
That leaves the reconnect/fallback path with weaker coverage for the same class
of bug where transport recovery appears healthy while the transcript remains
stale.

**Current behavior:**
- `ui/src/backend-connection.test.tsx` exercises the reconnect fallback flow.
- The test no longer proves that the session delta updates the visible preview,
  transcript, or store state while reconnect remains active.

**Proposal:**
- Restore an assertion against the visible preview, transcript text, or
  session-store state after the delta is dispatched.
- Keep the reconnect-state assertions so the test proves both facts: the UI is
  still recovering and the live delta was applied.

## Cross-window tab drag channel restarts on ordinary renders

**Severity:** High - `useAppDragResize` recreates its `BroadcastChannel` subscription on ordinary renders, which can drop cross-window tab drag coordination messages.

The extracted drag/resize hook now owns the `BroadcastChannel` that carries `drag-start`, `drop-commit`, and `drag-end` across windows. That effect depends on `applyControlPanelLayout`, but `App.tsx` still defines `applyControlPanelLayout(...)` inline, so its identity changes on every render. Any render while a cross-window tab drag is in flight tears down the active channel and creates a fresh one. Because the tab-drag protocol is transient and message-based, the source or target window can miss `drop-commit` / `drag-end`, leaving stale external drag state or failing to close the source tab after a successful cross-window drop.

**Current behavior:**
- `ui/src/app-drag-resize.ts` registers the tab-drag `BroadcastChannel` in a `useEffect` keyed by `applyControlPanelLayout`.
- `ui/src/App.tsx` recreates `applyControlPanelLayout(...)` on ordinary renders.
- A render during cross-window drag/drop can close and reopen the channel mid-protocol, dropping in-flight drag messages.

**Proposal:**
- Keep the channel effect stable per `windowId` instead of per render.
- Read the latest layout helper through a ref inside `channel.onmessage` rather than making it an effect dependency.
- Add a regression test that forces a render between `drag-start` and `drop-commit` and asserts the source tab still closes cleanly.



## Session store subscribers can dispatch stale action callbacks

**Severity:** High - `SessionBody` and `SessionComposer` now rerender from `session-store`, but their custom `memo` comparators still let mutating action callbacks stay frozen on older closures.

The new store-backed session slices in `ui/src/panels/AgentSessionPanel.tsx`
move transcript and composer data onto `useSessionRecordSnapshot(...)` /
`useComposerSessionSnapshot(...)`, while the panel and footer still receive
actions such as `onSend`, `onStopSession`, `onApprovalDecision`,
`onCancelQueuedPrompt`, `onRefreshSessionModelOptions`,
`onRefreshAgentCommands`, and `onSessionSettingsChange` from the parent. Unlike
the render callbacks, those actions were not wrapped in ref-backed adapters, yet
the `memo(...)` comparators still exclude them. That lets the store-driven UI
show fresh session data while user actions continue to execute through stale
closures captured before the latest parent render.

**Current behavior:**
- `SessionBody` and `SessionComposer` rerender from store subscriptions even
  when their parent props comparator short-circuits.
- The comparators at `ui/src/panels/AgentSessionPanel.tsx` omit mutating action
  props such as `onSend`, `onStopSession`, `onApprovalDecision`, and related
  refresh/settings callbacks.
- Those actions come from `useAppSessionActions(...)` closures that still read
  live lookups such as `sessionLookup`, `workspace`, and active session state,
  so invoking them through stale closures can target older app state than the UI
  currently renders.

**Proposal:**
- Treat mutating action props the same way the render callbacks are treated:
  either include them in the memo comparators or route them through stable
  ref-backed adapters inside `AgentSessionPanel`.
- Add focused regression coverage that updates the parent action closures while
  the store-backed composer/body rerender, then asserts send/approval/settings
  actions hit the latest session/workspace state.

## Session switches can briefly show the previous transcript and route actions to the new session

**Severity:** High - the refactor reuses one `SessionConversationPage` across session switches while still deferring `messages` and `pendingPrompts`, so the UI can briefly render stale cards from the previous session under the new session id.

`ui/src/panels/AgentSessionPanel.tsx` now keeps a single conversation page
mounted for the active session instead of keying or remounting that subtree per
session. At the same time, `SessionConversationPage` still runs
`useDeferredValue(session.messages)` and `useDeferredValue(pendingPrompts)`.
During a session switch, React can therefore keep the previous transcript data
alive for a deferred frame while `session.id` and the action bindings already
point at the newly selected session. That is no longer just a visual lag: cards
from session A can momentarily render while approval, queued-prompt, or
Codex-app actions are already bound to session B.

**Current behavior:**
- `SessionConversationPage` reuses the same component instance across session
  switches.
- `messages` and `pendingPrompts` are still deferred with `useDeferredValue(...)`.
- A session switch can render stale cards from the previous session for a
  deferred frame while action handlers are already bound to the newly selected
  `session.id`.

**Proposal:**
- Cut over immediately on `session.id` changes instead of deferring transcript
  arrays across session boundaries.
- Key the conversation subtree by `session.id`, or only use deferred
  message/prompt arrays when the session id is unchanged.
- Add a regression that switches sessions while the previous one has visible
  approval/pending cards and asserts no stale card is actionable under the new
  session id.

## Partial responses can consume restart detection before full snapshot adoption

**Severity:** High - the mutation-stamp fast path disables deep session
reconciliation only when `adoptState(...)` sees a server-instance change, but
partial create/fetch responses can update `lastSeenServerInstanceIdRef` first.

Backend `SessionRecord::mutation_stamp` values reset every process lifetime.
If a restarted backend returns a partial `CreateSessionResponse` or
`SessionResponse` before the next full `/api/state` snapshot, the partial
adoption updates `lastSeenServerInstanceIdRef`. The following full snapshot
then appears to come from the same instance, so `adoptState(...)` may leave the
mutation-stamp fast path enabled. Existing sessions whose stamps collide with
the previous process, especially stamp `0`, can keep stale client objects after
restart.

**Current behavior:**
- `adoptCreatedSessionResponse(...)` and `adoptFetchedSession(...)` can update
  `lastSeenServerInstanceIdRef` before a full state snapshot adopts.
- `adoptState(...)` computes `serverInstanceChanged` from that same ref.
- A full restart snapshot can therefore skip the deep reconcile that should
  run when process-local mutation stamps reset.

**Proposal:**
- Track "server instance changed since last full snapshot" separately from the
  latest instance id observed by any response.
- Or keep a full-state-specific server instance marker and compare full
  snapshots against that marker when deciding whether to disable the
  mutation-stamp fast path.
- Add App/live-state coverage where a partial response from instance B arrives
  before a full instance-B snapshot with colliding stamps, and assert the full
  snapshot still replaces stale session content.

## Session store publication can race ahead of React session state

**Severity:** Medium - the new `session-store` publishes some session slices before the corresponding React `sessions` state commits, so the UI can mix newer store-backed session data with older prop-derived session state in one render.

The staged refactor publishes `session-store` updates directly from
`ui/src/app-live-state.ts` and `ui/src/app-session-actions.ts`, while other
parts of the active pane still derive session data from React state in
`ui/src/SessionPaneView.tsx`. That leaves two live sources of truth on slightly
different timelines: `AgentSessionPanel` / `PaneTabs` can read the new store
snapshot immediately, while sibling props such as `commandMessages`,
`diffMessages`, waiting-indicator state, and other session-derived metadata are
still coming from the previous React `sessions` commit.

**Current behavior:**
- `session-store` is synced directly from live-state/action paths before some
  `setSessions(...)` commits land.
- `AgentSessionPanel` and `PaneTabs` read session data from the store.
- `SessionPaneView` still derives other active-session slices from React state,
  so the same active pane can render mixed-version session data within one
  update.

**Proposal:**
- Keep store publication aligned with committed React state, or finish moving
  the remaining active-session derivations in `SessionPaneView` onto the same
  store boundary.
- Document which layer is authoritative during the transition so later changes
  do not deepen the split-brain state model.
- Add an integration test that forces a store-backed session update plus a
  lagging React-state-derived sibling prop and asserts the active pane never
  renders a torn combination.

## `codexUpdated` SSE delta is missing contract documentation

**Severity:** Note - the backend and frontend now implement a `codexUpdated`
delta for Codex global state, but `docs/architecture.md` and the wire-level
comments do not document the new SSE payload.

`DeltaEvent::CodexUpdated { revision, codex }` is part of the current client
contract. Without documentation beside the other SSE delta variants, future
remote implementers and frontend maintainers have to infer the payload shape
from scattered Rust and TypeScript code.

**Current behavior:**
- `codexUpdated` is emitted and consumed as a valid SSE delta.
- The architecture docs still describe only the older delta variants.
- The wire comments do not call out the payload shape or intended usage.

**Proposal:**
- Add `codexUpdated` to the SSE delta contract in `docs/architecture.md`.
- Update the nearby `DeltaEvent`/wire comments to state that the payload is the
  latest `CodexState` plus the monotonic `revision`.

## Deferred heavy-content activation is coupled into the message-card renderer

**Severity:** Low - `ui/src/message-cards.tsx` now owns deferred heavy-content
activation policy in addition to Markdown, code, Mermaid, KaTeX, diff, and
message-card composition concerns.

The new provider/hook is useful, but keeping the virtualization activation
contract embedded in the same large renderer increases coupling between scroll
policy and message rendering. Future performance fixes will have to reason
through a broad module instead of a small boundary with a clear contract.

**Current behavior:**
- Deferred activation context, heavy Markdown/code rendering, and message-card
  composition live in one large module.
- Virtualization policy reaches into message rendering through exported
  activation context.
- The ownership boundary is not documented near the exported provider.

**Proposal:**
- Extract the deferred activation provider/hook into a focused module with a
  short contract comment.
- Consider extracting the heavy Markdown/code rendering path separately so
  virtualization policy and content rendering can evolve independently.

## `useState` initializer in `SessionComposer` writes to a shared ref

**Severity:** Medium - `ui/src/panels/AgentSessionPanel.tsx:788-806` includes `committedDraftsRef.current[initialSessionId] = initialCommittedDraft;` inside a `useState(() => { ... })` initializer. React's documentation explicitly warns against side effects in state initializers because the initializer can run more than once (StrictMode double-invoke, discarded concurrent renders). The write is idempotent today, but the pattern is a known footgun and any future code making the initialization non-idempotent would silently double-apply.

**Current behavior:**
- The `useState` initializer writes the initial committed draft into `committedDraftsRef.current[initialSessionId]`.
- Under React 18 StrictMode the initializer runs twice; the write is idempotent so no observable difference today.
- Any future non-idempotent logic (e.g. appending to an array, incrementing a counter, allocating a derived id) added to the initializer would silently double-apply.

**Proposal:**
- Move the `committedDraftsRef.current[initialSessionId] = initialCommittedDraft` write into the first `useLayoutEffect` keyed on `[activeSessionId]`.
- The state initializer can still compute the initial draft from `session?.committedDraft ?? ""` without writing the ref.

## `preferImmediateHeavyRender` is computed from a non-reactive ref during render

**Severity:** Medium - `ui/src/panels/VirtualizedConversationMessageList.tsx:666-667` computes the `preferImmediateHeavyRender` prop for `MeasuredPageBand` by reading `hasUserScrollInteractionRef.current` during render. Refs are not reactive, so the computed value only propagates when something else forces a re-render. Today that works because every scroll-event path that flips the ref to `true` also triggers `setViewport(...)` via `syncViewportFromScrollNode` within the same handler, which causes a re-render and re-reads the ref. But the coupling is implicit, undocumented, and brittle.

Any future scroll path that flips `hasUserScrollInteractionRef.current = true` without triggering a React state update will leave memoized pages with the stale `preferImmediateHeavyRender={true}` value until a different render trigger arrives — at which point heavy cards that should have stayed deferred will activate, defeating the purpose of the cooldown gate.

**Current behavior:**
- `preferImmediateHeavyRender` is computed each render from `hasUserScrollInteractionRef.current`.
- The ref is mutated in two handlers that also call `syncViewportFromScrollNode`, which updates `viewport` state and forces a re-render.
- If a future contributor adds a third setter without a matching state update, memoized pages will stay on a stale value.

**Proposal:**
- Promote `hasUserScrollInteraction` to component state (or state+ref pair), so every mutation triggers a re-render automatically.
- Alternatively, expose a helper like `setHasUserScrollInteraction(true)` that both writes the ref and calls a dedicated state-setter, and use that everywhere. Add a comment at the two existing setter sites naming the invariant.

## Composer switched to uncontrolled `<textarea>` without documenting the narrow-state contract

**Severity:** Medium - `ui/src/panels/AgentSessionPanel.tsx:1132-1172` migrated the composer from a controlled input (`value={composerDraft}`) to an uncontrolled input (`defaultValue={initialComposerDraft}`) with imperative `composerInputRef.current.value = ...` writes for everything but slash-prefixed drafts. The React state `composerDraft` is populated only when the current input starts with `/`; plain-text drafts live exclusively in the DOM and `composerDraft` reads `""`.

Today this is intentional — only the slash palette cares about the draft, and it only cares about slash drafts. But any future consumer of `composerDraft` through props, memo deps, or a derived state calculation will see empty strings for plain text and not understand why. The invariant "current draft text must be read via `getComposerDraftValue()`, not `composerDraft`" is implicit.

**Current behavior:**
- `<textarea>` is uncontrolled; `defaultValue={initialComposerDraft}` sets the first paint, imperative `ref.current.value = ...` handles later writes.
- `composerDraft` (React state) tracks only slash-prefixed drafts for palette rendering.
- Any non-slash reader of `composerDraft` observes empty strings for typed plain text.
- Nothing in code documents that current draft text MUST be read via `getComposerDraftValue()` / `composerInputRef.current?.value`.

**Proposal:**
- Add a block comment at the `currentLocalDraftState` declaration explaining the narrow slash-only meaning and that readers of current draft text MUST use `getComposerDraftValue()`.
- Rename `composerDraft` → `composerSlashDraft` (or `trackedSlashDraft`) to make the narrow purpose visible at every call site.

## `disableMutationStampFastPath` is not threaded through `sameSessionSummary`

**Severity:** Medium - `ui/src/session-reconcile.ts:76-110` passes `disableMutationStampFastPath` down to `reconcileSession(...)` but not to `sameSessionSummary(...)`. The summary comparator already checks `previous.sessionMutationStamp === next.sessionMutationStamp` directly (line 79). When the caller requests a deep reconcile (`disableMutationStampFastPath: true`) but the pre/post-restart summaries happen to be equal AND the stamps happen to collide (e.g. both `0` on fresh runtimes, or an unrelated u64 match), `sameSessionSummary` returns `true` and `reconcileSession` early-returns `previous` before `reconcileMessages` runs.

The existing test (`can disable the mutation-stamp fast path after a server restart` in `session-reconcile.test.ts`) forces a summary difference (preview text or messages length), so it passes even though the failure mode — summary-equal with colliding stamps and divergent messages — is the scenario the flag is supposed to address.

**Current behavior:**
- `sameSessionSummary(prev, next)` checks the stamp at line 79, outside any `options` gate.
- When `disableMutationStampFastPath` is `true` but summaries and stamps happen to match, the fast path is re-entered through a different door.
- The restart-divergent-transcript scenario is unprotected when summaries don't change.

**Proposal:**
- Pass `options` through to `sameSessionSummary` and, when `disableMutationStampFastPath` is `true`, treat the stamps as non-equal there too. Alternatively, factor the stamp check out of `sameSessionSummary` entirely and gate it in `reconcileSession` only.
- Add a regression test: two sessions with identical summaries, identical stamps, but diverging message arrays; assert that `disableMutationStampFastPath: true` causes `reconcileMessages` to produce the new messages.

## `CodexUpdated` delta carries a full subsystem snapshot despite the "delta" name

**Severity:** Medium - `src/wire.rs::DeltaEvent::CodexUpdated { revision, codex: CodexState }` publishes the entire `CodexState` on every rate-limit tick and every notice addition. The architectural contract the codebase otherwise respects is "state events for full snapshots, delta events for scoped changes". `CodexUpdated` is small today (rate_limits + notices capped at 5), but the naming invites future bulky additions to `CodexState` (login state, model-availability maps, per-provider metadata) to be broadcast in full on every tiny change.

**Current behavior:**
- The variant ships a full `CodexState` payload.
- Two publish sites in `src/session_sync.rs` send the complete snapshot even when only the rate limits changed.
- Wire name and shape set a precedent for "delta = tiny changes" that this variant violates.

**Proposal:**
- Split into narrower variants: `CodexRateLimitsUpdated { revision, rate_limits }` and `CodexNoticesUpdated { revision, notices }`. The two call sites in `session_sync.rs` already pick their publish trigger, so split dispatch is straightforward.
- Alternatively, add a source-level comment on the `CodexUpdated` variant stating that `codex` is intentionally the full subsystem snapshot and any future field addition to `CodexState` must reconsider whether a narrower event is needed.

## `CodexState.notices` 5-item cap is enforced in the mutator, not the type

**Severity:** Medium - `src/session_sync.rs::note_codex_notice` calls `notices.truncate(5)` to bound the notices vector, but the cap lives only at that call site. `CodexState.notices: Vec<CodexNotice>` in `src/wire.rs` declares no bound. A future caller that assembles a `CodexState` differently (constructing it directly, deserializing from a remote, or adding a second mutator) will bypass the cap and broadcast an unbounded vector over SSE.

**Current behavior:**
- `notices.truncate(5)` runs only inside `note_codex_notice`.
- Any other path that produces a `CodexState` is unconstrained.

**Proposal:**
- Extract a `const CODEX_NOTICE_CAP: usize = 5;` at module scope and use it at both the mutator and any future assembler. Document it in a doc comment on `CodexState.notices`.
- Alternatively, wrap the field in a newtype (`NoticeRingBuffer`) that enforces the cap on insertion.

## `DeferredHeavyContent` near-viewport activation now deferred by one paint

**Severity:** Low - `ui/src/message-cards.tsx:607-628` replaced `useLayoutEffect` with `useEffect` + a `requestAnimationFrame` before `setIsActivated(true)` for the near-viewport fast-activation branch. The previous sync layout-effect path activated heavy content that was already in-viewport before paint, avoiding a placeholder → content height jump. The new path defers activation by at least one paint, so on initial mount near the viewport the user may now see the placeholder for one frame before the heavy content replaces it. The deleted comment specifically warned about this risk for virtualized callers.

**Current behavior:**
- `useEffect` + `requestAnimationFrame` defers activation by ≥1 paint even when the card is already near viewport on mount.
- The deferral was added as part of the `allowDeferredActivation` cooldown gate (to avoid layout thrash during active scrolls).
- Near-viewport mount activation now produces a one-frame placeholder flicker in place of the previous zero-frame activation.

**Proposal:**
- Use `useLayoutEffect` when `allowDeferredActivation === true` (or for the near-viewport branch generally). Keep the `requestAnimationFrame` in the IntersectionObserver entry path for rapid-entry de-dupe.
- Alternatively, add a targeted comment explaining the deliberate trade-off if the new behavior is intended.

## Remote `DeltaEvent::CodexUpdated { .. }` arm uses a silent wildcard destructure

**Severity:** Low - `src/remote_routes.rs:1004-1013` matches `DeltaEvent::CodexUpdated { .. } => { ... }` in the remote dispatch loop. The `..` wildcard silently hides the `codex` field. If a future field is added to the variant, a reviewer walking the remote arm will not notice the new field is being dropped.

The intent ("process-global, not localized") is clearly documented in the comment and is correct — remote Codex state should not be absorbed locally. The hazard is purely in future-proofing.

**Current behavior:**
- `DeltaEvent::CodexUpdated { .. } => { /* no-op except revision bookkeeping */ }`.
- Adding a new field to the variant would not force a compiler or reviewer nudge at this call site.

**Proposal:**
- Use explicit-field destructure: `DeltaEvent::CodexUpdated { revision: _, codex: _ } => { ... }`. Adding a field becomes a compile error.
- Optionally add a doc comment on the variant in `wire.rs` clarifying the localization asymmetry.

## `"sessionId" in delta` poll-cancel branches are not extensible

**Severity:** Low - `ui/src/app-live-state.ts:1613, 1633` handle delta-event poll cancellations by structurally checking `"sessionId" in delta`. The two `revisionAction === "ignore"` / `"resync"` branches each hard-code the knowledge that only `SessionDeltaEvent` variants carry `sessionId`. Adding a third non-session delta type requires remembering to update both branches, and a new session-scoped delta that uses a different key (e.g. `sessionIds: string[]`) would silently miss both gates.

**Current behavior:**
- Two branches each run `"sessionId" in delta && typeof delta.sessionId === "string"`.
- The `SessionDeltaEvent` exclude type in `ui/src/live-updates.ts:76` exists but is not used here.

**Proposal:**
- Extract a `cancelPollsForDelta(delta: DeltaEvent)` helper that switches on `delta.type` (or uses the same `SessionDeltaEvent` narrowing). Call it from both branches.
- That also centralizes the "which deltas cancel which polls" contract in one place.

## `prevIsActive`-in-render replaced with post-commit effect delays the first-activation measurement pass

**Severity:** Low - `ui/src/panels/VirtualizedConversationMessageList.tsx:426-432` converted the `prevIsActive !== isActive` render-time derived-state update into a post-commit `useEffect`. Under the previous pattern, a session switching from `isActive: false → true` flipped `setIsMeasuringPostActivation(true)` during render, so the first frame rendered the measuring shell with the correct `preferImmediateHeavyRender` value. The new effect defers that flip to after commit — the first paint of the newly-active session briefly shows `isMeasuringPostActivation: false`, flipping to the measurement shell only on the next render.

Usually invisible (the effect runs the same tick). Under slow devices this may cause a one-frame flicker on session activation.

**Current behavior:**
- Post-commit effect fires after the first frame of the reactivated session.
- First paint uses `isMeasuringPostActivation: false` regardless of the actual transition.

**Proposal:**
- Restore the render-time pattern: `if (prevIsActive !== isActive) { setPrevIsActive(isActive); ... }` (the established React "derived state" form).
- Or upgrade the effect to `useLayoutEffect` so it runs before paint.
- The P2 task for `key={sessionId}` on the virtualizer supersedes this if that fix lands first.

## Focused live sessions monopolize the main thread during state adoption

**Severity:** Medium - a visible, focused TermAl tab with an active Codex session can spend multiple seconds of an 8 s sample on main-thread work even when no requests fail and no exceptions fire.

A live Chrome profile against the current dev tab showed no runtime exceptions, no failed network requests, and no framework error overlay, but the page still burned about `6.6 s` of `TaskDuration`, `0.97 s` of `ScriptDuration`, `372` style recalculations, and several long tasks above `2 s` while Codex was active. The hottest app frames were `handleStateEvent(...)` in `ui/src/app-live-state.ts`, `request(...)` / `looksLikeHtmlResponse(...)` in `ui/src/api.ts`, `reconcileSessions(...)` / `reconcileMessages(...)` in `ui/src/session-reconcile.ts`, `estimateConversationMessageHeight(...)` in `ui/src/panels/conversation-virtualization.ts`, and repeated `getBoundingClientRect()` reads in `ui/src/panels/VirtualizedConversationMessageList.tsx`. A second targeted typing profile pointed the same way: 16 simulated keystrokes averaged only about `1.0 ms` of synchronous input work and about `11 ms` to the next frame, while `handleStateEvent(...)` alone still consumed about `199 ms` of self time. Narrower composer-rerender and adoption-fan-out regressions have been fixed separately, but the remaining profile still points at broader whole-tab churn.

**Current behavior:**
- A visible, focused active session still produces repeated long main-thread tasks while Codex is working or waiting for output.
- Per-chunk session deltas now coalesce their full-session store publication and broad `sessions` render update to one animation frame, but full state snapshots and transcript measurement still need separate cuts.
- `codexUpdated` deltas and same-value backend connection-state updates are now coalesced or ignored, but snapshot adoption remains the dominant unresolved path.
- Slow `state` events now log per-phase timings in development, so the next profiling round should use the `[TermAl perf] slow state event ...` line to pick the next cut.
- Stale same-instance snapshots now avoid full JSON parse, so the remaining problematic lines should be adopted snapshots or server-restart/fallback snapshots.
- `handleStateEvent(...)` still drives broad adoption work through `adoptState(...)` / `adoptSessions(...)`, transcript reconciliation, and follow-on measurement/render work even after the narrower cleanup fan-out cut.
- `/api/state` resync currently reads full response bodies as text and runs `looksLikeHtmlResponse(...)` before JSON parsing, adding avoidable CPU on large successful snapshots.
- Transcript virtualization still spends measurable time on regex-heavy height estimation and synchronous layout reads, so live session churn compounds with scroll/measure work instead of staying isolated to the active status surface.

**Proposal:**
- Make the live state path more metadata-first so transcript arrays, workspace layout, and per-session maps are not reconciled or pruned when the incoming snapshot did not materially change those slices.
- Split the `/api/state` response handling into a cheap JSON-first path and keep HTML sniffing on a narrow error/prefix check instead of scanning whole successful payloads.
- Cache height-estimation inputs by message identity/revision and reduce repeated `getBoundingClientRect()` passes in the virtualized transcript.
- Re-profile the focused active-session path after each cut and keep this issue open until long-task bursts drop back below user-visible jank thresholds.

**Plan:**
- Start at the root of the profile: cut `handleStateEvent(...)` / `adoptState(...)` work first, because that is where both the passive and targeted rounds spend the most app CPU.
- Break the work into independently measurable slices: state adoption fan-out, `/api/state` parsing path, and transcript virtualization measurement/estimation.
- After each slice lands, rerun the live active-session profile and the focused typing round so reductions in `handleStateEvent(...)` self time, `TaskDuration`, and next-frame latency are verified instead of assumed.

## Prompt-settings pane can keep a stale render callback behind SessionBody memoization

**Severity:** Medium - `SessionBody` excludes `renderPromptSettings` from its memo comparator even though prompt mode calls it through a ref, so prompt settings can keep an outdated parent closure until some unrelated prop forces a rerender.

`ui/src/panels/AgentSessionPanel.tsx` memoizes `SessionBody` and intentionally
excludes the render callbacks from its comparator. That works for the message
renderers because the memoized subtree rerenders when their data props change,
but prompt mode calls `renderPromptSettingsRef.current(...)` directly. The ref
is only refreshed when `SessionBody` itself renders. If the parent recreates
the `renderPromptSettings` closure while the compared props stay equal, the
prompt-settings pane keeps using the stale callback.

**Current behavior:**
- `SessionBody` stores `renderPromptSettings` in a ref and reads that ref in
  prompt mode.
- The memo comparator explicitly excludes `renderPromptSettings`.
- Parent renders that only change the prompt-settings closure do not update the
  ref, so prompt mode can keep stale callback behavior.

**Proposal:**
- Include `renderPromptSettings` in the memo comparator, or wrap it in a stable
  ref-backed adapter that is refreshed outside the memo boundary.
- Add a focused regression that re-creates the prompt-settings renderer while
  the compared props stay equal and asserts prompt mode picks up the new
  closure immediately.

## Global Alt+PageUp/PageDown pane cycling outranks nested controls

**Severity:** Medium - the new window-level capture handler for `Alt+PageUp` / `Alt+PageDown` switches pane tabs before focused descendants can consume those shortcuts.

`ui/src/SessionPaneView.tsx` now installs a capture-phase `window` keydown
listener for `Alt+PageUp/PageDown` whenever the pane is active. Because it runs
above the focused widget boundary, nested editors, dialogs, or future
pane-local controls cannot opt into those shortcuts even when they own focus.
That makes the shortcut harder to scope and easier to break as more nested UI
surfaces land inside a session pane.

**Current behavior:**
- Active panes register a capture-phase `window` listener for
  `Alt+PageUp/PageDown`.
- The listener prevents default and switches pane tabs before descendants see
  the shortcut.
- Nested controls therefore cannot claim or suppress the combo from inside the
  active pane.

**Proposal:**
- Route the shortcut through the pane-root key handling path instead of a
  window-global capture listener.
- Or gate the capture listener with the same focused-target checks used for the
  other page-key routing so descendants can opt out.
- Add a focused regression with a nested focusable control that handles
  `Alt+PageUp/PageDown` and assert pane cycling does not preempt it.

## Composer drafts have three authoritative stores

**Severity:** Medium - committed composer drafts are tracked in React state (`draftsBySessionId`), a mutable ref (`draftsBySessionIdRef`), and the new `useSyncExternalStore`-backed `session-store`, with a post-commit effect mirroring state → ref and imperative paths writing the ref before React commits. Under concurrent draft updates the deferred effect can overwrite a newer ref value with a stale committed one, which then propagates to the composer snapshot via `syncComposerDraftForSession`.

`ui/src/session-store.ts` added a third source of truth for per-session drafts. Imperative handlers in `ui/src/app-session-actions.ts` (`handleDraftChange`, `sendPromptForSession`, queue-prompt flows) and `ui/src/app-workspace-actions.ts` write `draftsBySessionIdRef.current` synchronously before calling `setDraftsBySessionId`, so the store sync reads the fresh value. A separate effect in `ui/src/App.tsx` copies `draftsBySessionId` back into the ref after each commit. When two draft updates land in the same tick, the later-committed effect can briefly regress the ref to an older snapshot, and the store's composer-snapshot slice (`syncComposerDraftForSession`) can publish that stale draft to subscribers.

**Current behavior:**
- Three stores own the same data: React state, the ref, and the `session-store` slice.
- Imperative paths write ref → store before React commits; the effect writes state → ref after commit.
- Under concurrent updates the effect can stomp a newer imperative write with a stale React-committed value.

**Proposal:**
- Pick one owner for the ref: either drop the post-commit effect and rely entirely on imperative writes, or remove the imperative ref mutations and let the store read through a ref that mirrors state exactly once per commit.
- Document the invariant in the `session-store.ts` header so future changes do not reintroduce a third writer.
- Add a regression test that drives two overlapping `handleDraftChange` calls in the same tick and asserts the store snapshot matches the last-written value.

## `startActivePromptRecoveryPoll` only armed when adoption is stale

**Severity:** Medium - the recovery poll (renamed from `startActivePromptPoll`) previously armed on every successful `sendMessage` POST to cover the "POST acknowledged but SSE never streams" failure mode; it is now armed only when `adoptState(state)` returns false, removing the belt-and-suspenders check against silent SSE stalls following a successful POST.

`ui/src/app-session-actions.ts` narrowed the recovery poll to the stale-adoption branch. The SSE watchdog (`handleLiveSessionResumeWatchdogTick`) may already cover the "POST succeeded + SSE delta never lands" scenario, but no test exercises that path end-to-end after the change. The pre-existing `active-prompt-poll.ts` docblock explicitly mentions covering the post-POST silent-stall case — so either the watchdog provides equivalent coverage (in which case the comment is wrong), or the change removes a defense that wasn't duplicated elsewhere.

**Current behavior:**
- A successful `sendMessage` POST whose response the revision gate adopts never arms the recovery poll.
- Only stale POST responses (revision already exceeded by SSE) arm the poll.
- No regression test distinguishes "POST succeeds + SSE eventually streams" from "POST succeeds + SSE never streams".

**Proposal:**
- Add a test that mocks a successful POST followed by a blocked SSE stream and asserts the watchdog (or the recovery poll) eventually restores progress.
- If the watchdog provides the coverage, add a comment next to the new conditional documenting the reasoning and noting the `active-prompt-poll.ts` docblock that should be updated in step.
- If no other path covers it, restore the unconditional arm.

## `resolvePromptHistory` identity branches uncovered

**Severity:** Medium - `ui/src/session-store.ts::resolvePromptHistory` gates when the composer's `promptHistory` snapshot keeps object identity, but `session-store.test.ts` exercises only the happy-path branches. Three identity-determining branches — message-list shrinkage, boundary-id mismatch (in-place substitution/reorder), and equal-length "last message is a user prompt" — have no direct coverage, so a regression there would silently force the composer to re-render on every streaming delta.

`resolvePromptHistory` returns the previous `promptHistory` array (preserving identity) when the known-last-prompt still matches, and rebuilds a fresh array otherwise. Composer memoization depends on that identity preservation. If the function accidentally rebuilds on every call — or accidentally preserves identity when the history actually changed — the composer either re-renders on every assistant chunk or fails to refresh when the user edits history. Neither failure mode surfaces in the current tests.

**Current behavior:**
- Happy paths (assistant append, user prompt append, empty list) have tests.
- `nextLength < previousLength` (full recollect), `nextLength === previousLength` with mismatched boundary id (in-place edit/reorder), and the same-length-last-is-user-prompt passthrough are uncovered.

**Proposal:**
- Add one test per uncovered branch, asserting on both the returned value and its identity relative to the prior call's result.

## Session removal pruned only on the snapshot-adoption path

**Severity:** Low - `ui/src/session-store.ts` has no `removeSessionFromStore(...)` entry point, and the delta paths (`orchestratorsUpdated`, session-scoped deltas) only `upsertSessionSlice` for ids present in the delta. Today deltas cannot remove sessions, so this is latent — but the store has no defensive pruning and nothing in the file header documents which caller is responsible for eviction.

`syncComposerSessionsStore` handles pruning as a side effect of diffing `sessions[]`, so a full snapshot adoption cleans up orphans; the delta paths never do. If a future delta shape implies a session has been removed (e.g. a dropped slot in `mergeOrchestratorDeltaSessions`), the orphan slice would linger in `sessionRecordsById`, `sessionSummariesById`, and `composerSessionsById` until the next full snapshot.

**Current behavior:**
- Only `syncComposerSessionsStore` prunes the store; delta-scoped upserts never do.
- No documented contract in `session-store.ts` for which caller owns eviction.

**Proposal:**
- Add a `removeSessionFromStore(sessionId)` helper and wire it to the same places `setSessions` drops a session, or document the pruning contract in the `session-store.ts` header so future delta code knows to call `syncComposerSessionsStore` (or equivalent) when a session is removed.

## Runtime-only session mutation stamps can leak into persisted sessions

**Severity:** Low - `session_mutation_stamp` is now represented on the shared
`Session` wire struct, but that same struct is embedded in persisted
`PersistedSessionRecord` values.

The intended ownership is that `SessionRecord::mutation_stamp` is process-local
runtime metadata and `wire_session_from_record(...)` is the only outbound source
for the frontend-facing `sessionMutationStamp`. Remote proxy localization can
clone an inbound remote session payload into local `record.session`; if that
payload includes a remote process stamp, persistence can serialize it as part
of the local session. That does not break current behavior, but it blurs local
vs. remote stamp ownership and makes durable state carry a meaningless
process-local marker.

**Current behavior:**
- `Session` includes optional `session_mutation_stamp`.
- `PersistedSessionRecord` persists a `Session` value directly.
- Remote-localized sessions can arrive with a remote stamp unless every inbound
  path scrubs it.

**Proposal:**
- Clear `session_mutation_stamp` before persistence and after localizing inbound
  remote sessions.
- Keep `AppState::wire_session_from_record(...)` as the only path that sets the
  outbound stamp.
- Add a backend serialization/localization regression that proves persisted
  sessions do not contain `sessionMutationStamp`.

## `looksLikeHtmlResponse` 256-char slice drops leading-whitespace tolerance

**Severity:** Low - `ui/src/api.ts::looksLikeHtmlResponse(...)` now slices the first 256 raw characters before `trimStart().toLowerCase()`. A proxy or dev-server error page that emits more than 256 bytes of leading whitespace before `<html>` is no longer detected as HTML and falls through to `JSON.parse`, so the "Restart TermAl" guidance never surfaces for that response.

The bounded prefix probe is a performance improvement over the old "scan the whole body" behaviour, but the old code trimmed leading whitespace before inspecting the prefix, which this version does not. Realistic proxies are unlikely to emit >256 bytes of whitespace, but when they do the user sees a generic parse error instead of the restart prompt.

**Current behavior:**
- `raw.slice(0, 256).trimStart().toLowerCase()` — whitespace that exceeds 256 bytes pushes the `<html>` marker past the probe window.

**Proposal:**
- Reorder the operations: `raw.trimStart().slice(0, 256).toLowerCase()` — preserves the old semantics while still bounding the slice cost.

## `response.clone()` buffers every successful JSON response a second time

**Severity:** Low - the new JSON fast-path in `ui/src/api.ts::request(...)` calls `response.clone()` eagerly before parsing so the rare JSON-parse-failure branch can read the body as text. For large responses (full state snapshots, file reads, terminal-run transcripts) this doubles the memory footprint of every healthy JSON response just to preserve a fallback that is never consumed.

The existing `api.test.ts` case ("uses the JSON fast path for successful application/json responses") asserts the clone's `text()` call does NOT run on success — confirming the clone's buffered body is dead weight >99% of the time.

**Current behavior:**
- Every `response.ok` + JSON content-type path clones the response body immediately.
- The clone is only read in the catch branch, which the fast path almost never reaches.

**Proposal:**
- Consume `response.text()` once and `JSON.parse` the string, trading one allocation for avoiding the double-buffering. The HTML-sniff fallback becomes a plain branch on the already-materialised text string.

## `useDeferredValue(pendingPrompts)` receives a fresh `[]` each render

**Severity:** Low - `ui/src/panels/AgentSessionPanel.tsx:555-557` reads `session.pendingPrompts ?? []`, allocating a new empty array on every render when `pendingPrompts` is `undefined` (the common case). `useDeferredValue` treats each render as a changed value and schedules a transition even though the content is identical, which mildly defeats the purpose of deferring the value.

**Current behavior:**
- `pendingPrompts` is `session.pendingPrompts ?? []`.
- Every render when `pendingPrompts` is absent allocates a fresh array.
- `useDeferredValue` schedules a transition for identical empty content.

**Proposal:**
- Hoist a module-scope `const EMPTY_PENDING_PROMPTS: readonly PendingPrompt[] = [];` and use it as the fallback, so the deferred-value input has stable identity when the list is empty.

## Composer sizing double-resets on session switch

**Severity:** Low - `ui/src/panels/AgentSessionPanel.tsx:918-931` runs `resizeComposerInput(true)` synchronously inside a `useLayoutEffect` keyed on `[activeSessionId]`, and a following `useEffect` keyed on `[composerDraft]` schedules another resize via `requestAnimationFrame` on the same first render. The rAF resize is redundant because the synchronous one already measured the new metrics.

**Current behavior:**
- Layout effect resets cached sizing state and calls `resizeComposerInput(true)` synchronously.
- Draft effect schedules a second `requestAnimationFrame` resize on the same first render.
- First render of any newly-activated session does two resize passes instead of one.

**Proposal:**
- Track a "just-resized-synchronously" flag set in the layout effect and checked at the top of `scheduleComposerResize`, or gate the draft effect with a prev-draft ref so the "initial draft equals committed" case is a no-op.

## Composer autosize does not shrink on width-only pane resize

**Severity:** Low - the optimized composer resize path no longer forces a
shrink-capable measurement when only the textarea width changes.

`ui/src/panels/AgentSessionPanel.tsx` coalesces autosize work and only forces
the shrink-capable measurement (previously `height = "auto"`, now
`height = "0px"`) for session switches or panel-height changes. Widening a pane
can reduce text wrapping and therefore reduce the required textarea height, but
a width-only `ResizeObserver` update calls `scheduleComposerResize(...)`
without the force flag. The measured height can remain taller than its content
until another draft or session change triggers a full reset.

**Current behavior:**
- Pane width changes can alter wrapping without changing panel height.
- The resize scheduler does not force the textarea through a shrink-capable
  measurement pass for width-only changes.
- The composer can stay over-tall after widening the pane.

**Proposal:**
- Treat `widthChanged || panelHeightChanged` as a shrink-capable resize input,
  or otherwise force a shrink-capable measurement whenever wrapping can change.
- Add a focused test that widens the composer container and asserts the textarea
  height can shrink without requiring another keystroke.

## Duplicated `Session` projection types in `session-store.ts` and `session-slash-palette.ts`

**Severity:** Low - `ComposerSessionSnapshot` (`ui/src/session-store.ts:36-83`) and `SlashPaletteSession` (`ui/src/panels/session-slash-palette.ts:51-65`) each re-pick overlapping-but-non-identical field sets from `Session`. Three `Session`-like shapes now exist (`Session`, `ComposerSessionSnapshot`, `SlashPaletteSession`) with no compile-time check that additions to `Session` reach both projections — a new agent setting added to `Session` could silently default to `undefined` in consumers that read through either projection.

**Current behavior:**
- Both projection types declare field lists by hand.
- No `Pick<Session, ...>` derivation; nothing fails to compile when `Session` grows a new field.

**Proposal:**
- Derive both types via `Pick<Session, ...>`, or express `SlashPaletteSession` as `Omit<ComposerSessionSnapshot, ...>` where their field sets differ.
- Colocate the derivations in `session-store.ts` so the projection contract is visible in one place.

## `activeSessionId` vs. `session` dual identity in `SessionComposer`

**Severity:** Low - `ui/src/panels/AgentSessionPanel.tsx:791-797` computes `activeSessionId = session?.id ?? sessionId` so `activeSessionId` is truthy while the store is still catching up and `session` is still null. Other call sites guard differently (some check `!session`, some check `!activeSessionId || !session`). A future caller that guards only on `activeSessionId` will proceed with a null snapshot.

**Current behavior:**
- `activeSessionId` is truthy in a narrow window where `session` is still null.
- The two notions of "active" diverge in that window, and nothing documents the invariant.

**Proposal:**
- Either treat "snapshot null but id truthy" as "no session" (fall back to `null`), or add a comment near the fallback documenting that `activeSessionId` is a best-effort fallback and callers must still check `session` before reading capability fields.

## `session_message_count` silently saturates at `u32::MAX`

**Severity:** Low - `src/messages.rs:23-25` defines `session_message_count(record)` as `u32::try_from(record.session.messages.len()).unwrap_or(u32::MAX)`. A session with more than 4.29 billion messages silently reports `u32::MAX` rather than failing or surfacing the invariant violation. Practically unreachable (the process would OOM long before), but silent saturation defeats the Contract Precisions choice of `u32` over `usize` if the assumption ever breaks.

The frontend would then treat `4294967295` as truth, which would mis-represent session metadata in a way that's hard to diagnose.

**Current behavior:**
- `.unwrap_or(u32::MAX)` silently caps.
- No `debug_assert!` surfaces the assumption in test runs.
- No comment explaining the intentional saturation.

**Proposal:**
- Add `debug_assert!(record.session.messages.len() <= u32::MAX as usize)` above the conversion so tests catch the impossible.
- Alternatively, leave as-is but add a one-line comment explaining the intentional saturation so a future reviewer doesn't reach for checked arithmetic.

## HTTP-route tests leak persistence and orchestrator-template files into `temp_dir`

**Severity:** Medium - three new `tokio::test`s in `src/tests/http_routes.rs` leak `termal-test-*.json` and `termal-orchestrators-test-*.json` files into `std::env::temp_dir()` on every run. The tests move `state` into `app_router(state)` before capturing `state.persistence_path` / `state.orchestrator_templates_path`, so the `fs::remove_file` cleanup at the end of each test never executes.

Affected tests: `codex_thread_action_routes_update_session_state` (`:510`), `codex_thread_rollback_route_falls_back_when_history_is_unavailable` (`:670`), and `codex_thread_fork_route_returns_created_response` (`:757`). Over repeated local test runs this accumulates test artifacts that persist across development sessions.

**Current behavior:**
- Each test constructs `state`, reads fields off it, then moves `state` into `app_router(state)`.
- No file paths are captured before the move, so cleanup can't run.
- `%TEMP%` accumulates `termal-test-*.json` + `termal-orchestrators-test-*.json` files.
- Sibling tests in the same file that do clean up take the opposite approach (capture paths first).

**Proposal:**
- Clone `state` once at the top of each test (or destructure `state.persistence_path` and `state.orchestrator_templates_path` into local `PathBuf`s) before moving `state` into the router.
- In the cleanup block, call `fs::remove_file` on both the persistence path and the orchestrator-templates path.
- Extend the pattern to any other `http_routes.rs` tests that currently miss cleaning the orchestrator-templates file — this is a cross-cutting cleanup.

## SSE delta test doesn't pin `messageCount` on the delta payload

**Severity:** Low - `src/tests/http_routes.rs::state_events_route_streams_initial_state_and_live_deltas` (`:214-271`) asserts the SSE frame ordering (initial `state`, then `delta`) and the `type: "messageCreated"` discriminator, but does not assert `messageCount` on the delta payload itself. The snapshot test (`snapshot_bearing_routes_include_message_count`) pins `messageCount` on `StateResponse` / `SessionResponse`; the SSE delta wire contract is not pinned at the HTTP layer.

**Current behavior:**
- `snapshot_bearing_routes_include_message_count` pins `messageCount` on snapshot shapes.
- `state_events_route_streams_initial_state_and_live_deltas` asserts frame order and discriminator but not `messageCount` on the delta.
- The `tests/remote.rs` layer covers delta `messageCount` end-to-end, but the HTTP-level wire shape is unpinned for deltas.

**Proposal:**
- Add `assert_eq!(delta["messageCount"], 1);` (or an appropriate expected value) to `state_events_route_streams_initial_state_and_live_deltas` after the `type: "messageCreated"` assertion. One-line addition that closes the symmetry with the snapshot test.

## `docs/architecture.md` documents soft-rollout but not the `DeltaEvent` hard-break

**Severity:** Low - `docs/architecture.md:238-243` describes that `Session.messageCount` now rides on the wire with `#[serde(default)]` (soft rollout), but does not document the companion `DeltaEvent.*.messageCount` hard-break stance. A reader looking only at `architecture.md` cannot tell that mixed-version remote SSE bridges will hard-fail on missing `messageCount`. The policy is recorded in `docs/metadata-first-state-plan.md` Contract Precisions → Field semantics, but not cross-referenced from the architecture doc.

**Current behavior:**
- `architecture.md` describes the `Session.messageCount` soft-rollout.
- It does NOT mention that `DeltaEvent.*.messageCount` is required (no `#[serde(default)]`) and that mixed-version remote bridges are out of scope.
- `docs/metadata-first-state-plan.md:170-175` has the policy; `architecture.md` doesn't link to it.

**Proposal:**
- Add one sentence to the delta section: "Note: `DeltaEvent.*.messageCount` is required on the wire (no `#[serde(default)]`) — see `docs/metadata-first-state-plan.md` Contract Precisions → Field semantics for the intentional soft-rollout-on-Session + hard-break-on-Delta asymmetry."

## `#[cfg(test)] snapshot()` creates test-vs-production contract divergence

**Severity:** High - `src/state_accessors.rs:54-87` defines `snapshot()` as `full_snapshot()` in test builds and `summary_snapshot()` in production. Tests read `state.snapshot()` and rely on the full-transcript form (~40 sites in `src/tests/remote.rs` read `session.messages` directly from the snapshot), while production emits the summary shape. This is a silent behavioral split: a regression that flips a production code path from summary → full transcript (e.g. a new snapshot builder that forgets `wire_session_summary_from_record`) does not surface in any unit test that uses `snapshot()`.

Two HTTP-route contract tests (`snapshot_bearing_routes_include_message_count`) pin the route-level shape, but everything else in `src/tests/` continues to exercise a shape production never emits. The split was introduced as a transitional bridge so existing transcript-reading tests don't have to be rewritten, but the intent is invisible at every call site.

**Current behavior:**
- `#[cfg(test)] fn snapshot(&self) -> StateResponse { self.full_snapshot() }`
- `#[cfg(not(test))] fn snapshot(&self) -> StateResponse { self.summary_snapshot() }`
- ~40 test call sites rely on the full-transcript form.
- Two HTTP-route tests pin the production shape; everything else is legacy coverage.

**Proposal:**
- Rename the cfg-split helpers so intent is audible at every call site: tests explicitly call `full_snapshot_for_test()`; production paths explicitly call `summary_snapshot()`. Same body, different names.
- Or migrate the ~40 `state.snapshot().sessions[...].messages` call sites to read `state.inner.lock().sessions[...]` directly so tests assert against record state rather than wire state. Safer — transcript mutation checks don't route through the wire-projection helper at all.
- Either fix closes the latent cliff. The cfg-split without renames is deprecated.

## `resolvedWaitingIndicatorPrompt` duplicates `findLastUserPrompt` derivation across `SessionBody` and `SessionPaneView`

**Severity:** Low - `ui/src/panels/AgentSessionPanel.tsx:399-404` computes `resolvedWaitingIndicatorPrompt` by calling `findLastUserPrompt(activeSession)` inside `SessionBody` whenever the live turn indicator is showing, overriding the `waitingIndicatorPrompt` prop that `ui/src/SessionPaneView.tsx:795-805` already computed via the same helper and `useMemo`. The override was added to pick up store-subscriber updates between parent renders (correct intent), but it leaves two parallel code paths that must be kept in sync.

Two smaller concerns ride along:
- The override's condition includes an `"approval"` status arm (`status === "active" || status === "approval"`) that is presently unreachable: `SessionPaneView` only sets `showWaitingIndicator=true` when `status === "active"` or (`!isSessionBusy && isSending`), and `isSessionBusy` is true for `"approval"`, so `showWaitingIndicator && status === "approval"` never holds. Harmless defensive check but misleading for readers inferring the truth table.
- The resolution is not wrapped in `useMemo`, so it re-runs on every `SessionBody` re-render — once per streaming chunk. `findLastUserPrompt` scans from the tail, so it usually stops early, but sessions dominated by trailing tool/assistant output could scan deep.

**Current behavior:**
- `SessionBody` (`AgentSessionPanel.tsx:399-404`) and `SessionPaneView` (`SessionPaneView.tsx:795-805`) both derive the waiting-indicator prompt by calling `findLastUserPrompt(activeSession)` on the same store record.
- The override runs on every `SessionBody` render, uncached.
- The `status === "approval"` arm of the override's condition is unreachable under current upstream gating.

**Proposal:**
- Collapse to one computation at the store-subscriber boundary. Either `SessionBody` becomes the sole resolver (drop the `useMemo` and prop passthrough in `SessionPaneView`), or add a one-line cross-reference comment on both sites so future readers know the two are paired.
- Narrow the override's condition to `status === "active"` to match the upstream truth table.
- Wrap the override in `useMemo(() => findLastUserPrompt(activeSession), [activeSession.messages])` to avoid re-scanning on every streaming chunk.

## Conversation cards overlap for one frame during scroll through long messages

**Severity:** Medium - `estimateConversationMessageHeight` in `ui/src/panels/conversation-virtualization.ts` produces an initial height for unmeasured cards using a per-line pixel heuristic with line-count caps (`Math.min(outputLineCount, 14)` for `command`, `Math.min(diffLineCount, 20)` for `diff`) and overall ceilings of 1400/1500/1600/1800/900 px. For heavy messages â€” review-tool output, build logs, large patches â€” the estimate is 20Ã—-40Ã— under the rendered height, so `layout.tops[index]` for cards below an under-priced neighbour places them inside the neighbour's rendered area. The user sees the cards painted on top of each other for one frame, until the `ResizeObserver` measurement lands and `setLayoutVersion` rebuilds the layout.

An initial attempt to fix this by raising estimates to a single 40k px cap (and adding `visibility: hidden` per-card until measured) was reverted after it introduced two worse regressions: (1) per-card `visibility: hidden` combined with the wrapper's `is-measuring-post-activation` hide left the whole transcript empty for a frame whenever the virtualization window shifted before measurements landed; (2) raising the cap made the `getAdjustedVirtualizedScrollTopForHeightChange` shrink-adjustment huge (40k estimate â†’ 8k actual = âˆ’32k scrollTop jump), so slow wheel-scrolling through heavy transcripts caused visible scroll jumps of tens of thousands of pixels. The revert restores the one-frame overlap as the known limitation.

**Current behavior:**
- Initial layout uses estimates that badly under-price long commands / diffs.
- First paint places subsequent cards overlapping the under-priced one for one frame.
- Next frame, `ResizeObserver` fires, `setLayoutVersion` rebuilds, positions correct.
- Visible to the user as a brief "jumble" during scroll.

**Proposal:**
- Proper fix likely needs off-screen pre-measurement (render the card in a hidden measure-only tree, read `getBoundingClientRect` height, then place in the layout) rather than a formula-based estimate. This is a bigger change than a single pure-function tweak.
- Alternative: batch-measurement pass when the virtualization window shifts â€” hide the wrapper briefly, mount the newly-entering cards, wait for all their measurements, then reveal.
- Not: raise the estimator cap. Large overshoots trade one visible artifact for a worse one.


## Rendered Markdown diff view cannot jump between changes

**Severity:** Medium - regular file diffs expose previous/next change navigation, but the rendered Markdown diff view does not, so reviewing a long Markdown document requires manual scrolling and visual scanning.

This is especially noticeable because the same diff tab already has change navigation for the Monaco file-diff view. Switching to the rendered Markdown view removes that workflow even though the rendered segments already know which sections are added, deleted, or changed.

**Current behavior:**
- Monaco file diff shows change navigation controls and a `Change X of Y` counter.
- Rendered Markdown diff view shows highlighted added/deleted/changed sections, but does not expose next/previous change controls.
- Keyboard or toolbar navigation cannot jump between rendered Markdown change sections.

**Proposal:**
- Build a rendered-Markdown change index from the same segment model used to paint added/deleted/changed sections.
- Add previous/next controls and a `Change X of Y` counter for rendered Markdown mode, matching the regular diff affordance.
- Keep per-view scroll position stable when jumping or switching between Monaco and rendered Markdown diff views.
- Add coverage that rendered Markdown diff mode focuses/scrolls to the next and previous changed section without leaving the rendered view.

## Inline-zone id is line-number-dependent, reinitialises Mermaid diagrams on every edit above the fence

**Severity:** Medium - `ui/src/source-renderers.ts::detectMarkdownRegions` builds each Mermaid fence region's id as `mermaid:${fence.startLine}:${fence.endLine}:${quickHash(fence.body)}`. `startLine` and `endLine` are 1-based ABSOLUTE line numbers in the source buffer, so inserting any line above the fence shifts both â€” and the id flips. The `MonacoCodeEditor` portal is keyed on the zone id (see `MonacoCodeEditor.tsx:~718-730`); when the id flips, the portal unmounts and remounts, which tears down the Mermaid iframe and reinitialises it from scratch. Every keystroke in the heading / paragraphs above a Mermaid fence triggers this reinitialisation, producing a visible flicker on slow machines and wasting GPU cycles on fast ones.

The intent of the stable id was exactly the opposite â€” keep the diagram DOM alive across keystrokes outside the fence. A new test pinned the contract as it exists today (`SourcePanel.test.tsx::"inline-zone id stability" â†’ "changes the zone id when lines are inserted above the fence (latent stability gap)"`) so a future fix has a clear assertion to flip from `.not.toBe` to `.toBe`.

**Current behavior:**
- Id format: `mermaid:${startLine}:${endLine}:${hash(body)}`.
- Inserting a line above the fence shifts `startLine` â†’ id changes â†’ portal remounts â†’ Mermaid reinitialises.
- Typing inside the fence body changes the hash â†’ id changes â†’ portal remounts (correct â€” the diagram source changed).
- Editing below the fence (or in-place edits above without line-count changes) preserves startLine/endLine/body â†’ id stable (correct).

**Proposal:**
- **Primary**: drop `startLine`/`endLine` from the id and use `mermaid:${hash(body)}` alone. This preserves id stability under line shifts. The id must stay globally unique per file (the portal-key dedupe via `new Set(inlineZones.map((zone) => zone.id))` in `MonacoCodeEditor.tsx::zone-sync effect` collapses collisions into one entry, so non-unique ids would lose zones), which means a tiebreaker is needed ONLY when two fences collide on body hash. Tiebreaker rule: within a file, take the ordinal position of this fence among all fences that share its body hash, in document order (i.e., `mermaid:0:${hash}` for the first fence with this body, `mermaid:1:${hash}` for the second, etc.). Collisions are rare in practice; when they do happen, reordering two identical-body fences remounts both â€” semantically a no-op because identical bodies render identical diagrams.
- **Simpler but coarser alternative**: use `mermaid:${fenceOrdinal}:${hash}` where `fenceOrdinal` is the position among ALL Mermaid fences in the file (not just ones with the same body). This re-introduces a structural-remount problem the primary proposal avoids â€” inserting a new Mermaid fence BEFORE an existing one re-indexes every downstream fence and remounts them all. Listed for completeness; prefer the primary proposal.
- Flip the assertion in the test from `.not.toBe(idsBeforeEdit)` to `.toBe(idsBeforeEdit)` when the fix lands. Update the describe-header comment too â€” drop the "latent stability gap" paragraph once case (c) passes as "id stable".

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

## `shared_codex_thread_setup_persist_failure_does_not_tear_down_runtime` test flake

**Severity:** Low - `tests::shared_codex::shared_codex_thread_setup_persist_failure_does_not_tear_down_runtime` was observed failing intermittently during batched `cargo test --bin termal` runs. Passes when re-run in isolation. The two Gemini-auth siblings (`select_acp_auth_method_ignores_workspace_dotenv_credentials` and `gemini_dotenv_env_pairs_ignore_workspace_env_files`) were fixed by acquiring `TEST_HOME_ENV_MUTEX` and isolating HOME + Gemini/Google env vars; verified via 5 consecutive green `cargo test --bin termal` runs. The shared-codex test did not surface in those 5 runs, so either (a) it is much rarer than the Gemini one, (b) it was indirectly fixed by an unrelated change, or (c) it is still broken but the window is too narrow to hit.

**Current behavior:**
- Pass-in-isolation, fail-in-batch pattern when it surfaces.
- Unlike the Gemini flakes, this test does not obviously share HOME-rooted fixtures â€” likely a temp-file path collision or a side effect of persist-thread teardown.
- Has not surfaced in recent multi-run verification, so concrete reproduction is not yet captured.

**Proposal:**
- Reproduce via a regression harness that runs the test 20 times back-to-back under the full batch context; confirm the flake signature (temp-file collision vs env var vs persist-thread handle leak).
- If the flake is temp-file path collision: switch to `tempfile::tempdir()` with unique per-test directories.
- If env: add `TEST_HOME_ENV_MUTEX` acquisition and `ScopedEnvVar::remove` isolation to match the Gemini pattern.
- Document the root cause in the fix commit message so the "why mutex / why tempdir" is visible at review time.

## Server restart without browser refresh can lose the last streamed message

**Severity:** Medium - restarting the backend process while the browser tab is still open can make the most recent assistant message disappear from the UI, because the persist thread has a small window between "commit fires" and "row is durably in SQLite" during which an un-drained mutation is lost on kill.

Persistence is intentionally background and best-effort: every `commit_persisted_delta_locked` (and similar delta-producing commit helpers) signals `PersistRequest::Delta` to the persist thread and returns. The thread then locks `inner`, builds the delta, and writes. If the backend process is killed (SIGKILL, laptop sleep wedge, crash, manual restart of the dev process) between the signal fire and the SQLite commit, the mutation is lost. Old pre-delta-persistence behavior had the same window â€” the persist channel carried a full-state clone â€” so this is not a regression introduced by the delta refactor, but the symptom is visible now because the reconnect adoption path applies the persisted state with `allowRevisionDowngrade: true`: the browser's in-memory copy of the just-streamed last message is replaced by the freshly loaded (older) backend state, making the message disappear from the UI.

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

## SSE state broadcaster can reorder state events against deltas

**Severity:** Medium - under a burst of mutations, a delta event can arrive at the client before the state event for the same revision, triggering avoidable `/api/state` resync fetches.

Before the broadcaster thread, `commit_locked` published state synchronously (`state_events.send(payload)` under the state mutex), so state N always hit the SSE stream before any follow-up delta N+1. Now `publish_snapshot` enqueues the owned `StateResponse` to an mpsc channel and returns; the broadcaster thread drains and serializes on its own schedule. `publish_delta` remains synchronous. A caller that does `commit_locked(...)?` + `publish_delta(...)` can therefore race: the delta hits `delta_events` before the broadcaster drains state N. The frontend's `decideDeltaRevisionAction` requires `delta.revision === current + 1`; if state N hasn't advanced `latestStateRevisionRef` yet, the delta is treated as a gap and the client fires a full `fetchState`.

**Current behavior:**
- `publish_snapshot` is async (channel + broadcaster thread).
- `publish_delta` is sync.
- Client can observe delta N+1 before state N.
- Extra `/api/state` resync fetches fire under sustained mutation bursts.
- Correctness preserved (resync fixes the view), but behavior is chatty and pushes load onto `/api/state` â€” which is exactly the path we just made cheaper.

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

## SQLite persistence lacks file permission hardening and indefinite backup retention

**Severity:** Medium - session history including agent output, user prompts, and captured file contents is readable by other local users on default Unix systems, and a second sensitive copy is kept indefinitely at a predictable path.

The new SQLite persistence path opens `~/.termal/termal.sqlite` via `rusqlite::Connection::open` without setting restrictive permissions; on Unix, the default `umask 0022` yields world-readable `0644`. The JSONâ†’SQLite migration renames the legacy file to `sessions.imported-<timestamp>.json` (same permissions) and never deletes or surfaces it, so the full pre-migration history persists at a predictable path with no garbage collection or user notice.

**Current behavior:**
- `rusqlite::Connection::open` creates the DB with the current umask (0644 by default on Unix).
- `imported_json_backup_path` writes to a predictable directory alongside the DB.
- No GC, no UI notification of the backup path, no explicit "delete imported backup" action.

**Proposal:**
- On Unix, call `fs::set_permissions(path, Permissions::from_mode(0o600))` on both the SQLite DB and the imported backup immediately after open/rename.
- On Windows, document the reliance on `%USERPROFILE%\.termal\` ACL inheritance; optionally tighten via `SetNamedSecurityInfo`.
- Either delete the imported backup after a successful cold start confirms the SQLite file is usable, or emit a one-shot UI notice with the backup path and an explicit delete affordance.

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

## Lazy hydration effect: missing retry guard and unreconciled replace

**Severity:** Medium - the metadata-first hydration path still has two edge-case bugs around failed hydration and duplicate session materialization.

Two distinct issues remain in and around the one-shot `fetchSession` hydration path:
1. The async IIFE only guards against unmount. If the user switches away mid-fetch and the response's `session.id !== sessionId`, the code calls `requestActionRecoveryResyncRef.current()`. A transient server race can loop mismatch -> resync -> refetch -> mismatch.
2. `adoptCreatedSessionResponse` (and `live-updates.ts`'s `sessionCreated` reducer) raw-replace an existing session without per-message identity preservation via `reconcileSession`. If SSE `sessionCreated` materializes the session before the API response lands (or vice versa), memoized `MessageCard` children see new identities and remount.

**Current behavior:**
- The hydration effect is correctly keyed only by `activeSession?.id` and `activeSession?.messagesLoaded`, but the mismatch branch still triggers action-recovery resync without a "tried once" marker.
- Raw `[...previousSessions, created.session]` / `replaceSession(..., delta.session)` on the `existingIndex !== -1` branch.

**Proposal:**
- Add a `hydrationMismatchSessionIdsRef` (or count attempts) to avoid re-firing after one mismatch until an authoritative state event arrives.
- Route the existing-session replace branch through `reconcileSession` (or a similar identity-preserving merge) so memoized children keep stable identity.
