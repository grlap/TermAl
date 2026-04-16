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
- **Stripped diff preview document restore after hydration** — restored Git diff preview tabs are now scanned after workspace layout readiness and on every ready pane-tree change, not just on initial mount, and request-key guards prevent duplicate document-content restore fetches.
- **Manual Git diff request-key guard** — manual Git status diff opens now increment and check the same monotonic request-key generation as restore and file-watch refreshes, with App coverage proving a late first open cannot overwrite a reopened diff tab.
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
- **Rendered Markdown committer registry churn** — `DiffPanel` now passes stable ref-forwarded rendered-Markdown callbacks into `MarkdownDiffView`, and coverage asserts that typing in one section does not remount sibling editable sections.
- **Pasted rendered-Markdown skip attributes** — editable rendered-Markdown sections now sanitize pasted HTML by removing `data-markdown-*` trust attributes before insertion, and coverage asserts pasted `data-markdown-serialization="skip"` content is still saved.
- **Rendered Markdown paste sanitizer** — editable rendered-Markdown paste now intercepts arbitrary HTML, keeps only a Markdown-oriented element allowlist, strips active attributes and unsafe URLs, drops embedded/SVG/MathML content, and has regression coverage for malicious clipboard HTML without TermAl serialization markers.
- **Mermaid render budget** — Markdown rendering now skips Mermaid diagrams over 50,000 source characters or documents with more than 20 Mermaid fences, falling back to source display with regression coverage for both limits.
- **Mermaid SVG isolation** — rendered Mermaid SVG now loads in a sandboxed iframe instead of being inserted with `dangerouslySetInnerHTML` in the app DOM, with coverage proving malicious SVG markup stays out of TermAl's DOM.
- **Editable Mermaid render-budget coverage** — `DiffPanel` now exercises an oversized Mermaid fence through the editable rendered-Markdown diff path, asserting the budget warning and preserved source stay visible without calling Mermaid.
- **Diff view scroll slots** — changed-only and raw diff views now attach real scroll refs, edit mode exposes a Monaco code-editor scroll handle, and switching among non-default diff views restores their prior offsets.
- **Git diff refresh version reset** — diff refresh versions are now kept monotonic for the browser process lifetime, so closing a diff tab cannot reset the guard while an older fetch for the same request key is still in flight.
- **Restored diff preview App-level coverage** — `App` now has integration coverage for stripped Git diff tabs restored from workspace layout hydration, including request payloads, hydrated document content, propagated enrichment notes, restore failure `loadError`, duplicate-fetch prevention, and late responses after unmount.
- **Rendered Markdown documentContent draft rebase** — active rendered-Markdown DOM drafts now keep the segment and source document from the start of the edit, avoid React reconciliation while dirty, and rebase the saved range when refreshed `documentContent` shifts earlier content.
- **Repeated Markdown diff chunk identity** — rendered Markdown views now reuse previous segment ids across document refreshes by matching content plus nearby structural context, so inserting an identical upstream chunk no longer steals the downstream repeated section's draft identity.
- **Rendered Markdown commit-batch atomicity** — rendered Markdown commits now keep section drafts dirty until the parent accepts the batch, abort the whole batch on any unmappable section, reject overlapping resolved ranges before applying splices, and flush active DOM drafts before apply-to-disk rebases so partial, stale, or garbled saves cannot clear local drafts.
- **DiffPanel save adapter options** — the App-level `DiffPanel` save adapter now forwards `baseHash` and `overwrite` into `handleSourceFileSave`, and App coverage proves stale-save conflict recovery keeps the base hash and sends `overwrite: true` on the save-anyway path.
- **`useStableEvent` layout-phase freshness** — `DiffPanel` now updates stable-event refs in `useLayoutEffect`, with a comment documenting the `flushSync` event-path requirement, so synchronous layout-phase handlers no longer have a post-paint stale-callback window.
- **Untagged degraded Markdown enrichment notes** — `git_diff_document_enrichment_note` now returns a generic user-visible note for untagged `BAD_REQUEST` and `NOT_FOUND` degradation paths, with coverage for both statuses.
- **Structured diff scroll restore target** — `StructuredDiffView` now attaches the changed-only scroll ref and test id to the actual `.structured-diff-body` scroller, so saved offsets are read and restored from the visible scroll container.
- **Lazy session hydration stale-revision guard** — full-session fetches now add a session id to `hydratedSessionIdsRef` only when `adoptFetchedSession` accepts the response, so stale lower-revision responses do not permanently block a later retry.
- **Lazy session hydration cleanup guards** — `adoptSessions` now prunes `hydratingSessionIdsRef` and `hydratedSessionIdsRef` when sessions disappear, and `handleRefreshSessionModelOptions` returns before synchronous state mutations if `App` is already unmounted.
- **SQLite import cleanup visibility** — if JSON-to-SQLite import succeeds but renaming the legacy JSON file fails, cleanup failure for the incomplete SQLite file is now logged instead of silently discarded.
- **SQLite connection busy/WAL settings** — production SQLite connections now share an opener that sets a 5-second busy timeout plus `journal_mode = WAL` and `synchronous = NORMAL` before schema or persistence work.

## State snapshots still serialize full session transcripts

**Severity:** High - large conversation histories make `/api/state`, SSE state snapshots, and reconnect/restore paths spend CPU serializing messages that the list view usually does not need.

`snapshot_from_inner_with_agent_readiness` still clones every visible `Session` with its full `messages` vector into `StateResponse`. The HTTP `/api/state` handler and full-state SSE publisher then serialize those full transcripts even when the frontend only needs session metadata. With long Codex/Claude conversations this pushes hot CPU into `serde_json` and makes reconnects, tab restore, and any full-state publish scale with total transcript size.

**Current behavior:**
- `/api/state` returns all visible sessions with all historical messages.
- `publish_state_locked` builds the same full transcript snapshot for full-state SSE events.
- The dedicated `GET /api/sessions/{id}` route exists, but state snapshots do not defer to it.

**Proposal:**
- Make state snapshots metadata-first: include session shell fields and mark transcript-bearing sessions as `messagesLoaded: false` with an empty `messages` array.
- Keep `GET /api/sessions/{id}` as the authoritative full-transcript route, and keep session-create/prompt flows returning enough data that the active prompt UI remains reliable.
- Add backend and App-level regression coverage proving `/api/state` omits transcripts, session hydration restores the full transcript, and metadata snapshots do not clear an already-hydrated active session.

## Workspace layout saves rewrite every persisted session

**Severity:** Medium - frequent layout autosaves can keep the persistence thread busy serializing all visible session transcripts even though only workspace metadata changed.

`put_workspace_layout` calls `commit_locked`, which calls `persist_internal_locked`. That path builds a full `PersistedState::from_inner`, including every visible `PersistedSessionRecord`, and sends it to the persistence worker. In production SQLite this eventually serializes and upserts every session row. Dragging tabs, changing panes, or other layout churn should update only workspace metadata, but currently scales with total transcript size.

**Current behavior:**
- `put_workspace_layout` stores one `WorkspaceLayoutDocument` and then commits through the full-state persistence path.
- `PersistedState::from_inner` includes all visible sessions, so unchanged transcripts are serialized again.
- The SQLite storage layer has split session rows, but layout-only commits do not use a metadata-only persistence path.

**Proposal:**
- Add a layout/metadata-only persistence request that updates app metadata without serializing or upserting unchanged session rows.
- Consider moving workspace layouts into their own SQLite table so layout writes never touch session records.
- Add a regression or instrumentation test that a workspace layout save does not serialize session messages.

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

## SQLite storage still opens a fresh connection per operation

**Severity:** Low - the first SQLite slice is restart-safe, but it leaves avoidable connection churn and small repeated schema writes in the hot path.

Production SQLite connections now set `busy_timeout`, `journal_mode = WAL`, and `synchronous = NORMAL`, so ordinary lock contention and fsync cost are bounded better than the initial implementation. The storage helpers still open a fresh connection for each load/full persist/create persist, and `ensure_sqlite_state_schema` still writes `schema_version` on every open.

**Current behavior:**
- `load_state_from_sqlite` and `persist_state_parts_to_sqlite` each open a fresh SQLite connection.
- `ensure_sqlite_state_schema` still runs the schema-version upsert on every connection open.
- Eager `.or(...)` in `load_state_from_sqlite` double-queries app state on the happy path.

**Proposal:**
- Share a single `Arc<Mutex<Connection>>` on `AppState` for prepared-statement caching and reduced connection churn.
- Check `schema_version` before upserting so ordinary opens avoid a metadata write.
- Convert `.or(...)` to lazy `if let Some(..) else { .. }`.

## Remote proxy `applied_remote_revision` path skips broadcast of non-session state changes

**Severity:** Medium - orchestrator, project, and other-session changes pulled from a remote during proxy-session create/fork are persisted but never broadcast via SSE.

When `create_remote_session` or `fork_remote_codex_thread` sees `applied_remote_revision == true`, the path now calls `bump_revision_and_persist_locked` (no state snapshot publish) plus `publish_delta(DeltaEvent::SessionCreated)` for the newly created session only. But `apply_remote_state_if_newer_locked` can mutate projects, orchestrators, and other sessions as part of the remote snapshot application. Those changes get the revision bump but ride along without any SSE notification — previously they were published by `commit_locked` as a full state snapshot. Clients have stale views of non-session slices until the next unrelated commit.

**Current behavior:**
- `applied_remote_revision` branch calls `bump_revision_and_persist_locked` + publishes `SessionCreated` only.
- Any non-session slice mutated by `apply_remote_state_if_newer_locked` (projects, orchestrators, other sessions) is silently absent from the SSE stream.

**Proposal:**
- When `applied_remote_revision` is true, retain a full-snapshot publish path (publishes a state event) in addition to the SessionCreated delta, or issue additional deltas for the non-session slices that changed.
- Add a regression test: fork a remote Codex thread with an orchestrator change in the snapshot; assert the orchestrator change reaches the local SSE stream.

## `CreateSessionResponse` contract does not guarantee at least one of `session` or `state`

**Severity:** Medium - the TS adapter `adoptCreatedSessionResponse` silently returns `false` when both are absent; a protocol-drift bug in the backend can silently lose a newly created session until the next state resync.

`CreateSessionResponse` now has `session?`, `revision?`, and `state?` all optional with no type-level invariant. Every existing Rust handler populates `session + revision`, but the struct does not enforce the contract. If a future handler ships a `{sessionId}`-only response, the frontend's `adoptCreatedSessionResponse` returns `false`, the session is not added, and the UI silently loses the creation until SSE or a state resync catches up.

**Current behavior:**
- Rust: `#[serde(default, skip_serializing_if = "Option::is_none")]` on `session` and `state`; `#[serde(default)]` on `revision`.
- Frontend: `session?: Session | null; revision?: number; state?: StateResponse | null`.
- `adoptCreatedSessionResponse` has a final `return false` branch when neither is present.

**Proposal:**
- Model as `#[serde(untagged)]` enum with two variants: `{ sessionId, session, revision }` or `{ sessionId, state }`. Both variants carry an enforced invariant.
- Or document the "session is always populated by every handler in this tree" contract in the Rust struct doc and add a test parsing a `{sessionId}`-only payload to fail if a handler ever drifts.

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

## Remote `SessionCreated` publishes a duplicate revision on the no-op branch

**Severity:** Low - remote proxy session create/fork with `!applied_remote_revision && !changed` publishes a `SessionCreated` delta with a non-advanced revision, weakening the monotonic-revision invariant.

In `create_remote_session` and `fork_remote_codex_thread`, when the remote snapshot is not newer AND the local proxy record already exists, the code still calls `publish_delta(DeltaEvent::SessionCreated { revision: inner.revision, ... })` without bumping the revision. Downstream, `App.tsx` writes `latestStateRevisionRef.current = delta.revision` unconditionally, so the same revision value is written twice. Benign today but violates the "each delta advances revision" contract.

**Current behavior:**
- `!applied_remote_revision && !changed` branch: `revision = inner.revision` (no bump).
- `publish_delta` runs anyway.
- Frontend accepts the duplicate-revision write.

**Proposal:**
- Skip `publish_delta` when neither `applied_remote_revision` nor `changed` is true.
- Or always bump the revision before publishing so every delta carries a strictly-increasing value.

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

## Mermaid sandbox iframe has no maximum dimensions

**Severity:** Medium - a Mermaid diagram from agent output or repository Markdown can force a multi-thousand-pixel iframe in the app layout.

`getMermaidDiagramFrameStyle` derives the iframe's `width` and `height` from the SVG `viewBox` with only lower bounds (`Math.max(120, …)` / `Math.max(320, …)`). `Number.isFinite` rejects NaN/Infinity, but a legitimately large flowchart (or an attacker-crafted SVG with a pathologically large `viewBox`) produces tens of thousands of pixels of iframe. The `.mermaid-diagram-frame` CSS rule sets `max-width: none`, so the iframe overflows its parent column and disrupts scrolling and layout. The iframe is sandboxed (no XSS risk), but the UI is still a layout DoS target.

**Current behavior:**
- `readMermaidSvgDimensions` parses the `viewBox` width/height verbatim from the SVG source.
- `getMermaidDiagramFrameStyle` returns `{ width: Math.max(320, Math.ceil(w) + 2)px, height: Math.max(120, Math.ceil(h) + 2)px }` with no upper cap.
- `.mermaid-diagram-frame` CSS uses `max-width: none`, amplifying overflow.

**Proposal:**
- Cap the computed `width` and `height` at a sane maximum (e.g., `Math.min(4096, …)` or a container-sized cap via `100%` + `aspect-ratio`).
- Tighten `.mermaid-diagram-frame` to `max-width: 100%` so the iframe scales with its parent.
- Add a regression fixture with a huge `viewBox` that proves the iframe's rendered dimensions stay bounded.

## `hasOverlappingMarkdownCommitRanges` misses zero-length and touching ranges

**Severity:** Low - two rendered Markdown commits that resolve to the same zero-length insertion point pass the overlap check and apply in non-deterministic order.

`hasOverlappingMarkdownCommitRanges` checks `current.range.start < previous.range.end` against ranges sorted ascending by start. For two commits resolving to identical zero-length ranges `[10, 10)`, `current.start === previous.end === 10`, so `10 < 10` is false and both pass. The subsequent reduce sorts descending by start with a stable sort, so both inserts at position 10 apply in input order, with the second insert ending up before the first in the resulting string. For strictly adjacent ranges like `[5, 20)` and `[20, 25)` the descending-by-start sort keeps non-overlapping splices independent, but the zero-length degenerate case can silently garble content.

**Current behavior:**
- `hasOverlappingMarkdownCommitRanges` uses strict `<` comparison on start/end.
- Two zero-length resolved ranges at the same position pass.
- The reduce applies both at the same insertion point; result depends on input ordering.

**Proposal:**
- Treat zero-length ranges that share a position as overlapping, or reject any case where the resolved ranges are not strictly disjoint (`current.start <= previous.end` when at least one is zero-length).
- Add direct regression coverage for two rendered Markdown sections resolving to the same zero-length insertion point and assert the batch is rejected with the existing save-error banner.

## `git_diff_document_enrichment_note` duplicates the untagged-degradation status list

**Severity:** Low - the list of degradable untagged status codes is now maintained independently in two helpers and can drift.

`should_degrade_git_diff_document_enrichment_error` and the new fallback arm in `git_diff_document_enrichment_note` both encode the `BAD_REQUEST | NOT_FOUND` untagged-degradation set. A future contributor adding a new degradation status (e.g., `UNPROCESSABLE_ENTITY`) can update only one and break the invariant that every degraded response carries a visible note.

**Current behavior:**
- Both helpers match untagged errors against `BAD_REQUEST | NOT_FOUND` using local literal patterns.
- No shared constant or single source of truth for the status list.

**Proposal:**
- Share a `const DEGRADED_UNTAGGED_STATUSES: &[StatusCode]` slice (or a helper) and use it in both functions.
- Add a regression that iterates the shared list and asserts `should_degrade_git_diff_document_enrichment_error` and `git_diff_document_enrichment_note` agree on every status in it.

## Implementation Tasks

- [ ] P2: Add metadata-only state snapshot coverage:
  backend tests should assert `/api/state` omits transcript payloads while
  `GET /api/sessions/{id}` still returns the full transcript. App tests should
  assert a metadata snapshot preserves an already-hydrated active session and
  does not disrupt prompt input or focus.
- [ ] P2: Add workspace-layout persistence hot-path coverage:
  exercise a `put_workspace_layout` update with a large visible session and
  assert the layout save path does not serialize or upsert unchanged session
  rows/messages.
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
  (`ui/src/App.test.tsx:2031-2179`): the test relies on the first
  `fetchGitDiff` call returning `staleDiffDeferred.promise` and the
  second returning `currentDiffDeferred.promise`. A future code change
  that adds any intermediate fetch silently swaps the mapping. Replace
  with `mockImplementation((req) => ...)` that keys off a
  request-correlated field (e.g., call counter + filePath) so the
  deferred mapping is explicit.
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
