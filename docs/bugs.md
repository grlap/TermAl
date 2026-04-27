# Bugs & Known Issues

This file tracks reproduced, current issues. Resolved work, speculative refactors,
cleanup notes, implementation task ledgers, and external limitations do not belong here.

## Active Repo Bugs

## `SessionBody` re-wraps render callbacks already stabilized by `AgentSessionPanel.useRenderCallback`

**Severity:** Medium - `ui/src/panels/AgentSessionPanel.tsx:416-435`. The new `useRenderCallback` hook in `AgentSessionPanel` (lines 206-208) now produces stable identities for `renderCommandCard`, `renderDiffCard`, and `renderMessageCard`. But `SessionBody` still hand-rolls the same `useRef + useCallback` indirection for the same three callbacks. The comparator comment at lines 552-559 still describes them as "inline closures whose identity changes every render" — which is no longer true.

The round's stated goal was to "formalize the render-phase ref pattern" with a single named hook. The inner pattern wasn't removed, leaving three callback patterns coexisting (`useStableEvent`, `useRenderCallback`, plus inline ref-wraps in `SessionBody`). This is the opposite of formalization.

**Current behavior:**
- `AgentSessionPanel` wraps three callbacks in `useRenderCallback` → stable identity.
- `SessionBody` re-wraps the same three (now-stable) callbacks via internal `useRef + useCallback` → no behavior change, just dead weight.
- Comparator's exclusion comment refers to the old inline-closure semantics.

**Proposal:**
- Drop the inner `renderMessageCardRef`/`renderCommandCardRef`/`renderDiffCardRef` blocks in `SessionBody` and pass props through directly (they're already stable from the parent).
- Update the comparator comment: "AgentSessionPanel stabilizes these via `useRenderCallback` — they're already reference-equal across renders, but excluded from the comparator anyway because the comparator exists for prop-identity flow, and equality on stable identities is a no-op."
- Keep `renderPromptSettingsRef` — it's distinct because the outer panel does NOT stabilize `renderPromptSettings` and the comparator depends on identity comparison for prompt mode.

## `useRenderCallback` hook lacks documentation despite living next to `useStableEvent`

**Severity:** Low - `ui/src/panels/AgentSessionPanel.tsx:125-131`. The new `useRenderCallback` hook has zero JSDoc. Its sibling `useStableEvent` (in `ui/src/panels/use-stable-event.ts`) has a thorough header explaining ownership, the layout-effect ordering rationale, and what it does NOT own. The two hooks have near-identical signatures and ~95% identical bodies but differ in their publication point — `useLayoutEffect` (post-commit) vs render-phase ref-mutation (pre-layout) — which matters for callees invoked during the same render.

A future contributor sees two near-identical hooks living next to each other and won't know which to reach for.

**Current behavior:**
- `useStableEvent` has a header comment.
- `useRenderCallback` has none.
- The decision rule for choosing between them lives only in the file-level comparator comment.

**Proposal:**
- Add a header comment matching `use-stable-event.ts`'s style: render-phase ref mutation; intended for callbacks invoked synchronously during render (e.g., `renderMessageCard(message)` inside a `.map()`); not safe for `flushSync`-driven event handlers (use `useStableEvent` for those).
- Optionally split into `panels/use-render-callback.ts` for findability and side-by-side discovery with `use-stable-event.ts`.

## `renderPromptSettings` inline arrow at `SessionPaneView.tsx:3147` defeats the prompt-mode comparator

**Severity:** Low - `ui/src/SessionPaneView.tsx:3147` × `ui/src/panels/AgentSessionPanel.tsx:550-551`. The `SessionBody` comparator now checks `(previous.viewMode !== "prompt" || previous.renderPromptSettings === next.renderPromptSettings)`. When `viewMode === "prompt"`, this compares prop identity. But the upstream `renderPromptSettings` prop is defined as a fresh inline arrow function in `SessionPaneView.tsx` on every render, so the comparator returns `false` on every parent render in prompt mode.

The whole point of the comparator is to avoid re-rendering `SessionBody` on unrelated parent updates. In prompt mode it does the opposite — re-renders every time.

**Current behavior:**
- Prompt-mode comparator branch defeats memoization due to inline-arrow prop.
- Non-prompt mode is unaffected.

**Proposal:**
- Either (a) decide prompt-mode re-rendering on every parent render is intentional and document it (comment at the inline arrow site).
- Or (b) wrap `renderPromptSettings` in `useRenderCallback` in `AgentSessionPanel` the same way the other three render callbacks are wrapped, and leave the comparator gate as a safety net.

## `MARKDOWN_INTERNAL_LINK_HREF_ATTRIBUTE` constant has no doc comment for the cross-file contract

**Severity:** Low - `ui/src/markdown-links.ts:71`. The new exported constant `MARKDOWN_INTERNAL_LINK_HREF_ATTRIBUTE = "data-markdown-link-href"` underpins a cross-file contract: the renderer in `ui/src/message-cards.tsx` writes this attribute when scrubbing a workspace-link href; the serializer in `ui/src/panels/markdown-diff-edit-pipeline.ts` reads it before falling back to `href` for round-trip preservation.

Without a doc comment, a future maintainer cleaning up "unused" data-attributes could break the editor's link round-trip silently. Tests would catch it, but the contract should be discoverable from the constant itself.

**Current behavior:**
- Constant declared and exported with no documentation.
- Two consumers in different files rely on the attribute name as a string.
- The round-trip purpose lives only in commit-message context.

**Proposal:**
- Add a 3-4 line doc comment on the constant: "Renderer writes the original href into this attribute when scrubbing the visible `href` to `#`; serializer reads it first when re-emitting `[text](href)` so workspace-file links round-trip through the rendered-Markdown editor without dropping the destination. Both `message-cards.tsx` and `markdown-diff-edit-pipeline.ts` import this symbol — keep the contract on this name."

## `shouldScrubMarkdownDomHref` has no direct unit test

**Severity:** Low - `ui/src/markdown-links.ts:195`. The new exported predicate has multiple regex branches (`file://`, `/[A-Za-z]:[\\/]`, `[A-Za-z]:[\\/]`, decoded variants). It's currently covered only via integration through the new `MarkdownContent.test.tsx` test "scrubs encoded Windows file-link hrefs even when source resolution fails".

A regex regression that broke one branch (e.g., dropping the `safeDecodeMarkdownHref` chain) would only be caught if a `MarkdownContent` test happened to exercise that exact input shape.

**Current behavior:**
- Helper has multiple branches.
- Coverage is integration-only via one specific test.
- No direct unit-level coverage in `markdown-links.test.ts`.

**Proposal:**
- Add a small `describe("shouldScrubMarkdownDomHref")` block in `markdown-links.test.ts` covering each branch: raw `file://`, raw drive letter, encoded drive letter, slash-prefixed encoded drive letter, plus negative cases like `https://...` and `#anchor`.

## No regression test for hostile `data-markdown-link-href` round-trip

**Severity:** Low - `ui/src/panels/markdown-diff-edit-pipeline.test.ts`. The serializer reads `data-markdown-link-href` FIRST and falls back to `href`. Logic correctly rejects unsafe protocols via `isSerializableMarkdownHref`, but a regression that bypassed the protocol check on the data-attribute branch (e.g., if someone "fixed" the round-trip path to skip validation) would not be caught.

The existing safety test (line 807) only exercises the `href` path with `javascript:alert(1)`. The new round-trip test (line 825) only uses a benign `C:\repo\docs\README.md` href.

**Current behavior:**
- Hostile-protocol rejection works at the `href` attribute path.
- The newer `data-markdown-link-href` precedence path is not pinned for hostile inputs.

**Proposal:**
- Add a test that builds an editable section with `<a href="#" data-markdown-link-href="javascript:alert(1)">Unsafe</a>` and asserts `serializeEditableMarkdownSection` returns just `"Unsafe"` (no link).

## `useRenderCallback` not pinned by a "latest renderer fires after handler-only rerender" test

**Severity:** Low - `ui/src/panels/AgentSessionPanel.test.tsx`. The new "refreshes prompt settings when only the prompt renderer changes" test exercises the `renderPromptSettings` path, which is NOT bridged via `useRenderCallback` (it's directly compared by the comparator). The `renderMessageCard`/`renderCommandCard`/`renderDiffCard` paths use `useRenderCallback` and aren't tested for the "after rerender, the latest renderer is invoked" path.

A regression that froze `useRenderCallback` to the initial closure would silently break visible rendering — children would keep showing the original render output even after the parent passes a new render function.

**Current behavior:**
- `useRenderCallback` has no direct test.
- `renderPromptSettings` (the non-`useRenderCallback` path) is tested.
- `renderMessageCard`/`renderCommandCard`/`renderDiffCard` (the `useRenderCallback` paths) are not.

**Proposal:**
- Add a test that rerenders with a new `renderMessageCard` returning visibly different DOM ("Initial card" → "Latest card") and asserts the latest text appears, mirroring the prompt-renderer test but for the `useRenderCallback`-bridged path.

## `seenServerInstanceIdsRef` unbounded set growth is undocumented

**Severity:** Low - `ui/src/App.tsx:490` × `ui/src/app-live-state.ts:355-362`. `rememberServerInstanceId` only `.add()`s to `seenServerInstanceIdsRef`; there's no eviction or cap. Each entry is ~36 chars (UUID); growth is bounded by "number of distinct server-restart UUIDs in this tab's lifetime".

For Phase 1 single-user local that bound is small enough to be irrelevant (1000 restarts ≈ 50KB). But the new contract has no documented upper bound and no eviction strategy. The architecture-review brief explicitly asked us to flag if not capped.

**Current behavior:**
- Set grows monotonically over the tab's lifetime.
- No documented bound or eviction strategy.
- No comment near the ref initialization explaining why unboundedness is acceptable.

**Proposal:**
- Either (a) document the boundedness assumption next to `useRef<Set<string>>(new Set())` at `App.tsx:490`: "Bounded set; older ids never evict because the only consumer (`shouldAdoptSnapshotRevision`) just needs to recognize already-seen instance ids during the current tab's lifetime."
- Or (b) cap the set at a small bound (e.g., 64 entries) using insertion-order eviction. A `Map<string, never>` works as an ordered-set replacement.

## `codex_thread_actions.rs:156-164` has `if let Some(index)` followed by trailing `expect()` — contradictory invariant claims

**Severity:** Low - `src/codex_thread_actions.rs:156-164`. The block uses `if let Some(index) = inner.find_session_index(&record.session.id)` (silent no-op on lookup failure) but the trailing `.expect("just-created Codex session must be present in the index")` at line 171 panics on the same condition. The two patterns express opposite beliefs about the same invariant.

Verified upstream: `inner.create_session(...)` (in `state_inner.rs:175`) calls `self.push_session(record.clone())` synchronously, so the index is always populated before `find_session_index` runs. Both should be `expect()`.

**Current behavior:**
- `if let Some(index) = ...` silently no-ops if lookup fails.
- The trailing `.expect()` panics in the same case.
- The two branches contradict each other; the silent fallback is dead code.

**Proposal:**
- Replace `if let Some(index) = ...` at line 156 with `let index = inner.find_session_index(...).expect("just-created Codex session must be present in the index")`, mirroring the trailing assertion.
- This makes both branches honest about the invariant and avoids the silent no-op.

## `shouldScrubMarkdownDomHref` doesn't include UNC paths in its predicate

**Severity:** Low - `ui/src/markdown-links.ts:202-208`. The predicate covers `file://`, `/[A-Za-z]:[\\/]`, `[A-Za-z]:[\\/]`, plus URI-decoded variants, but does not match `^\\\\` (UNC paths like `\\server\share\file.md`).

UNC paths are caught indirectly: `looksLikeAbsoluteMarkdownFilePath` (in the same file, line 319) returns true for `^\\\\`, so `resolveMarkdownFileLinkTarget` produces a non-null `fileLinkTarget`, which already forces `scrubDomHref = true` in `message-cards.tsx`. No exposure under the current renderer pipeline. But the predicate's coverage is non-obvious from its name — its safety relies on the resolver's UNC branch, not on the predicate itself.

**Current behavior:**
- UNC paths are scrubbed via `fileLinkTarget` resolution path.
- `shouldScrubMarkdownDomHref` doesn't match them directly.
- Predicate's defense-in-depth coverage is incomplete from its name.

**Proposal:**
- Add `/^\\\\/.test(normalizedHref)` and `/^\\\\/.test(decodedHref)` to `shouldScrubMarkdownDomHref` so the predicate's coverage matches its comment-level intent.
- Aligns with the absolute-path detection in `looksLikeAbsoluteMarkdownFilePath` and removes the implicit dependency on resolver behavior.

## `hydrationRetainedMessagesMatch` "extra fields" test only pins matcher strictness, not Message type purity

**Severity:** Low - `ui/src/app-live-state.test.ts:78-86`. The test casts a fake field via `as Message` to simulate the bypass: `{ messages: [{ ...message, localRenderCache: true } as Message] }`. This pins the matcher's strictness against synthetic divergence — but a future legitimate `Message` field added to `BaseMessage` (`ui/src/types.ts:310`) would be present on both sides; `JSON.stringify` would still match; the contract would silently drift.

The source-side comment at `ui/src/app-live-state.ts:225-227` warns about this, but the comment is remote from where the change would be made (the `Message` type definition).

**Current behavior:**
- Test rejects synthetic extra fields via type cast.
- The contract relies on a comment in the matcher file, far from `BaseMessage` where new fields would be added.
- A legitimate `Message` field added to both sides would silently bypass the matcher.

**Proposal:**
- Either (a) add a separate "persisted-message projection" function (`pickPersistedMessageFields(message)`) used by both sides of the comparison, with a brand or lint check pinning its field list.
- Or (b) put a comment on `BaseMessage` itself in `ui/src/types.ts` warning that any new field there must also be reflected in `hydrationRetainedMessagesMatch`'s persistence contract.
- Optionally add a snapshot assertion on `Object.keys(message)` against an explicit allowlist tied to `Message`'s persisted shape.

## `app-live-state.ts` past 1,500-line review threshold for TypeScript utility modules

**Severity:** Low - `ui/src/app-live-state.ts`. File is now 2,474 lines after this round's `+38` net additions. The architecture rubric §9 sets a pragmatic ~1,500-line threshold for TypeScript utility modules. The hydration cluster (`hydrationRetainedMessagesMatch`, `SESSION_HYDRATION_RETRY_DELAYS_MS`, `SessionHydrationTarget`, `SessionHydrationRequestContext`) is now a clean extraction candidate — well-defined contract, existing direct unit-test coverage, no React-component dependency.

**Current behavior:**
- Single module mixes hydration matching, retry scheduling, profiling, JSON peek helpers, and the main state machine.
- Per-cluster grep tax growing with each round.

**Proposal:**
- Defer to a dedicated pure-code-move commit per CLAUDE.md.
- Extract `hydration-retention.ts` (or `session-hydration.ts`) containing `hydrationRetainedMessagesMatch`, `SESSION_HYDRATION_RETRY_DELAYS_MS`, `SessionHydrationTarget`, `SessionHydrationRequestContext`, and the matching unit tests.

## `useAppDragResize` test file covers 1 of 7 returned handlers

**Severity:** Low - `ui/src/app-drag-resize.test.tsx`. This is the FIRST test file for the 560-line `useAppDragResize` hook, but provides limited coverage. The single test asserts BroadcastChannel stability across rerenders and one `drop-commit` message path. Most public surface is untested: `handleSplitResizeStart` (pointer events drive split-ratio updates), `handleTabDragStart`/`End`, `handleControlPanelLauncherDragStart`/`End`, and `handleTabDrop` placement variants.

The test was specifically a regression test for cross-window drag-channel stability and is well-scoped to that, but the file's existence shouldn't be misread as comprehensive coverage.

**Current behavior:**
- `useAppDragResize` has one regression test for `BroadcastChannel` stability + `drop-commit`.
- 6 of 7 returned handlers are untested.

**Proposal:**
- Add `it` blocks per handler covering the basic happy path: split resize via pointer events, tab drag start/end, control-panel launcher drag, and tab drop placement variants (same pane, different pane, last tab edge case).

## `AgentSessionPanel.test.tsx` past 5,000-line review threshold

**Severity:** Low - `ui/src/panels/AgentSessionPanel.test.tsx`. File is now 5,659 lines (+511 this round), past the project's review threshold for test files. The added blocks cluster naturally by concern — composer memo coverage, scroll-following coverage, ResizeObserver fixtures — and would extract cleanly into siblings without behavioral change.

The adjacent `App.live-state.*.test.tsx` split (April 20) is the precedent for per-cluster `.test.tsx` files. Per `CLAUDE.md`, splits must be pure code moves and live in their own commit.

**Current behavior:**
- Single `AgentSessionPanel.test.tsx` mixes composer, scroll, resize, and lifecycle clusters.
- Per-cluster grep tax growing with each replay-cache-adjacent feature round.

**Proposal:**
- Pure code move: extract into `AgentSessionPanel.composer.test.tsx`, `AgentSessionPanel.scroll.test.tsx`, `AgentSessionPanel.resize.test.tsx` (matching the App.live-state cluster shape).
- Defer to a dedicated split commit; do not couple with feature changes.

## `markdown-diff-change-section.tsx` clipboard/Range helpers should extract to a sibling module

**Severity:** Low - `ui/src/panels/markdown-diff-change-section.tsx`. The file grew 724 → 860 lines this round (+180). Four new helpers — `setDropCaretFromPoint`, `getSelectionRangeInsideSection`, `rangeCoversNodeContents`, and `serializeSelectedMarkdown` — form a cohesive cluster (range/selection geometry + clipboard serialization) with no React-component dependency. Per CLAUDE.md, the project is "actively splitting" rather than growing the existing large files.

The current size (~860 lines) is below the 2,000-line review threshold for TSX components, but the cluster is exactly the kind of natural extraction boundary the project's "pure code move" pattern was set up for. Current file header now has to describe both the per-section component and the clipboard plumbing, blurring its contract.

**Current behavior:**
- Four pure DOM/Range helpers live alongside the React components.
- The file header has to cover both responsibilities.
- Future clipboard-pointer-geometry work would continue to widen the file.

**Proposal:**
- Pure code move: extract the four helpers into `ui/src/panels/markdown-diff-clipboard-pointer.ts` in a dedicated commit.
- Add a header comment to the new file explaining what it owns + provenance.
- Update the change-section file header to drop the clipboard plumbing references.
- Keep React event handlers (`handleCopy`, `handleCut`, `handleDrop`) in `markdown-diff-change-section.tsx`.

## `src/tests/remote.rs` past the 5,000-line review threshold

**Severity:** Low - `src/tests/remote.rs` is now 9,202 lines after this round's +471-line addition, well past the project's review-threshold for test files. The new replay-cache work clusters cohesively between lines ~2,810 and ~4,040 (the `RemoteDeltaReplayCache` shape helper, the `local_replay_test_remote` / `seed_loaded_remote_proxy_session` / `assert_delta_publishes_once_then_replay_skips` / `assert_remote_delta_replay_cache_shape` / `test_remote_delta_replay_key` helpers, and the `remote_delta_replay_*` tests).

The growth is incremental across many rounds of replay-cache hardening, not a single landing — but extracting the cluster keeps the rest of the file's per-test density manageable. Per `CLAUDE.md`, splits must be pure code moves and live in their own commit.

**Current behavior:**
- Single `src/tests/remote.rs` mixes hydration tests, orchestrator-sync tests, replay-cache tests, and protocol-shape tests.
- Per-cluster grep is harder than necessary; future replay-cache work continues to grow the file.

**Proposal:**
- Extract the replay-cache cluster (lines ~2,810–4,040) into `src/tests/remote_delta_replay.rs` as a pure code move — including the helpers and all `remote_delta_replay_*` tests.
- Defer to a dedicated split commit; do not couple with feature changes.

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

## Repeated `commit_locked` boilerplate at four early-exit paths in `hydrate_remote_session_target`

**Severity:** Low - `src/remote_routes.rs:561-606`. The new `remote_state_applied` flag must be checked and `commit_locked` called before each of four early-exit paths. The same `if remote_state_applied { commit_locked(...).map_err(...)?; } return Err(...)` boilerplate is duplicated. A future maintainer adding a fifth early-exit could easily forget the conditional commit, reintroducing the watermark-race bug under a new condition.

**Current behavior:**
- The conditional commit-before-error is replicated four times in close proximity.
- No helper or invariant comment near the function body documents that every error-return must commit broad state when applied.

**Proposal:**
- Extract a small helper closure (`bail`) that takes an error and conditionally commits before returning.
- Or invert the flow so the broad-state apply runs after the can-this-target-survive checks, eliminating the need to commit-before-error.

## Unloaded remote proxy hydration has no timeout/fallback

**Severity:** Medium - `src/state_accessors.rs:151-181` performs a synchronous outbound HTTP fetch + possible remote `/api/state` resync when `GET /api/sessions/{id}` hydrates an unloaded remote proxy. The contract is now documented, but a slow or wedged remote still stalls every visible-pane hydration request and every reconnect resync; a remote that returns `messages_loaded: false` returns `bad_gateway` to the local client instead of degrading to the unloaded summary.

**Current behavior:**
- `get_session()` performs synchronous remote HTTP I/O in the unloaded-proxy branch.
- The hydration call publishes a global SSE state event via `commit_locked`, fanning out to every connected client.
- `bad_gateway` propagates to the caller when the remote returns metadata-only or refuses; no fallback to the local unloaded summary.

**Proposal:**
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

## `CodexUpdated` delta carries a full subsystem snapshot despite the "delta" name

**Severity:** Medium - `src/wire.rs::DeltaEvent::CodexUpdated { revision, codex: CodexState }` publishes the entire `CodexState` on every rate-limit tick and every notice addition. The architectural contract the codebase otherwise respects is "state events for full snapshots, delta events for scoped changes". `CodexUpdated` is small today (rate_limits + notices capped at 5), but the naming invites future bulky additions to `CodexState` (login state, model-availability maps, per-provider metadata) to be broadcast in full on every tiny change.

**Current behavior:**
- The variant ships a full `CodexState` payload.
- Two publish sites in `src/session_sync.rs` send the complete snapshot even when only the rate limits changed.
- Wire name and shape set a precedent for "delta = tiny changes" that this variant violates.

**Proposal:**
- Split into narrower variants: `CodexRateLimitsUpdated { revision, rate_limits }` and `CodexNoticesUpdated { revision, notices }`. The two call sites in `session_sync.rs` already pick their publish trigger, so split dispatch is straightforward.
- Alternatively, add a source-level comment on the `CodexUpdated` variant stating that `codex` is intentionally the full subsystem snapshot and any future field addition to `CodexState` must reconsider whether a narrower event is needed.

## `DeferredHeavyContent` near-viewport activation now deferred by one paint

**Severity:** Low - `ui/src/message-cards.tsx:607-628` replaced `useLayoutEffect` with `useEffect` + a `requestAnimationFrame` before `setIsActivated(true)` for the near-viewport fast-activation branch. The previous sync layout-effect path activated heavy content that was already in-viewport before paint, avoiding a placeholder → content height jump. The new path defers activation by at least one paint, so on initial mount near the viewport the user may now see the placeholder for one frame before the heavy content replaces it. The deleted comment specifically warned about this risk for virtualized callers.

**Current behavior:**
- `useEffect` + `requestAnimationFrame` defers activation by ≥1 paint even when the card is already near viewport on mount.
- The deferral was added as part of the `allowDeferredActivation` cooldown gate (to avoid layout thrash during active scrolls).
- Near-viewport mount activation now produces a one-frame placeholder flicker in place of the previous zero-frame activation.

**Proposal:**
- Use `useLayoutEffect` when `allowDeferredActivation === true` (or for the near-viewport branch generally). Keep the `requestAnimationFrame` in the IntersectionObserver entry path for rapid-entry de-dupe.
- Alternatively, add a targeted comment explaining the deliberate trade-off if the new behavior is intended.

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
