# Bugs & Known Issues

This file tracks reproduced, current issues. Resolved work, speculative refactors,
and cleanup notes do not belong here.

## Active Repo Bugs

Also fixed in the current tree, not re-listed below:

- **Stale `PersistRequest::Full` documentation** — `src/app_boot.rs` persist-thread inline comment and `src/persist.rs::persist_delta_via_cache` doc-comment no longer reference the removed `Full` variant or the deleted `persist_state_via_cache` helper. Both now describe the current delta-only signal shape directly and cross-reference `StateInner::collect_persist_delta` as the authoritative description of what gets written. `src/state.rs::PersistRequest` lost its stray "Tracks app state." stub preamble while keeping the substantive doc.
- **New GET `/api/sessions/{id}` endpoint and `DeltaEvent::SessionCreated`/`MessageCreated` variants undocumented in architecture.md** — the REST endpoint table at `docs/architecture.md` lists `GET /api/sessions/{id} -> SessionResponse { revision, session }` and the documented `DeltaEvent` list covers `SessionCreated` and `MessageCreated` alongside the streaming deltas.
- **High-risk backend subsystems lack invariant-level documentation** — added contract docs to the previously under-documented hot spots: `sync_remote_state_inner` / `apply_remote_state_if_newer_locked` (snapshot vs focused sync, revision gate, id localization, rollback scope, mutation stamps, callers), the `orchestrators.rs` file-level block (template vs instance storage, full lifecycle, file boundary with `orchestrator_lifecycle.rs` / `orchestrator_transitions.rs`), `acp.rs` (strict initialize → authenticate → session/load-or-new → set_mode/model → prompt handshake, pending JSON-RPC request ownership, timeouts, session-load fallback), and `build_instruction_search_graph` (seed discovery, path canonicalization, reference sanitization, transitive-edge BFS with cycle guard, skipped directories, document classification). Dozens of low-value `/// Handles ...` stubs on trait-impl forwarders and enum accessors were removed or replaced with substantive docs; the remaining `/// Handles ...` in `turn_lifecycle.rs` is a real description rather than a stub.
- **Architecture.md project structure + state-mutation pattern stale after refactor** — the `## Backend` state dump now lists SQLite storage + the `file_events` broadcaster + the `persist_tx` channel; the state-mutation pattern block documents the background `termal-persist` thread draining `PersistRequest::Delta` signals, `StateInner::collect_persist_delta` as the under-lock diff builder, the mutation-stamp invariant routed through `session_mut*` / `push_session` / `remove_session_at` / `retain_sessions`, and `commit_delta_locked` as the delta-only commit; the project structure listing now reflects the full post-refactor file layout (state/boot, sessions/turns, agents, HTTP API, wire DTOs, remote proxies, orchestrators).
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
- **SQLite persistence connection reuse** — the background persist thread now owns a `SqlitePersistConnectionCache`, reusing one SQLite connection and running `ensure_sqlite_state_schema` only on the first open for the process lifetime. The per-persist hot path no longer pays for opening a fresh connection or upserting `schema_version` on every commit.
- **Mermaid iframe dimension cap** — `getMermaidDiagramFrameStyle` now clamps the derived `viewBox` width and height to 4096 px and the iframe CSS uses `max-width: 100%`, so a pathologically large Mermaid diagram (agent output or hostile Markdown) cannot produce a thousand-pixel iframe that overflows its parent column.
- **Zero-length rendered Markdown commit overlap detection** — `hasOverlappingMarkdownCommitRanges` now rejects two zero-length ranges that share an insertion point, and a zero-length range touching a non-empty sibling, while still allowing strictly adjacent non-empty ranges. Two rendered Markdown sections resolving to the same position no longer silently garble the saved document.
- **Shared untagged-degradation status list** — `should_degrade_git_diff_document_enrichment_error` and `git_diff_document_enrichment_note` now share a single `DEGRADED_UNTAGGED_STATUSES` constant, with a regression that iterates every listed status and asserts both helpers agree (the degradable set flips to note-producing and vice versa).
- **Delta persistence** — the background persist thread now writes only the sessions whose monotonic `mutation_stamp` advanced past its watermark. `StateInner` exposes `session_mut` / `session_mut_by_index` / `stamp_session_at_index` / `push_session` / `remove_session_at` / `retain_sessions` helpers that stamp on access, and every production `&mut inner.sessions[..]` / `push` / `remove` / `retain` site in `src/state.rs`, `src/remote.rs`, and `src/orchestrators.rs` is routed through them. `commit_locked` no longer clones `PersistedState::from_inner` under the state mutex; it just sends a `PersistRequest::Delta` signal. The persist thread briefly re-locks `inner`, runs `collect_persist_delta` (clones only changed sessions + drains `removed_session_ids` + metadata-only clone), and writes via `persist_delta_via_cache` with targeted `INSERT OR UPDATE` per changed session and `DELETE WHERE id = ?` per removed id — no `DELETE FROM sessions` sweep. Workspace layout saves, preference updates, and any commit that does not touch sessions now rewrite only the `app_state` metadata row instead of every session row.
- **Broadcaster thread for SSE state** — `publish_snapshot` now sends an owned `StateResponse` through a dedicated `termal-state-broadcast` thread that serializes and forwards to `state_events`. The state mutex is no longer held during JSON serialization.
- **`get_state` serializes inside `spawn_blocking`** — `GET /api/state` now builds the snapshot and runs `serde_json::to_vec` in the same `run_blocking_api` closure and returns a prebuilt `axum::response::Response` with `Content-Type: application/json`. No tokio runtime worker sits on synchronous JSON serialization of the full state.
- **`persist_internal_locked` single-clone path** — the persist channel now carries only a `PersistRequest::Delta` signal; the `PersistedState::from_inner` clone that used to run under the state mutex is gone. The test-build sync fallback rebuilds `PersistedState` only when the broadcaster channel is disconnected (unit tests).
- **Self-chained safety-net poll** — the post-send `/api/state` watchdog is a chained `setTimeout(30_000)` instead of `setInterval(3000)`, so slow responses can no longer stack overlapping inflight requests when the server is busy.
- **`sync_remote_state_inner` stamps remote-proxy updates** — the remote-state sync loop now collects `(index, remote_session_id, local_project_id)` in an immutable first pass, then re-borrows via `inner.session_mut_by_index(idx)` for each update so every applied remote change bumps `mutation_stamp`. Remote-proxy rows are now picked up by the SQLite delta persist on every remote-state snapshot instead of being silently skipped at the watermark.
- **`remove_project` stamps the affected sessions** — project deletion now collects the affected session indices first, then clears `session.project_id` through `session_mut_by_index` so the cleared rows land in SQLite on the next persist tick. Previously, the in-memory `project_id = None` never reached disk and deleted projects would reappear attached to those sessions on restart.
- **`create_session` + `create_session_from_fork` re-stamp after slot replace** — both code paths now call `inner.session_mut_by_index(index)` after `*slot = record.clone()` so the whole-struct replace no longer erases the stamp that `push_session` applied. Without this, the row was invisible to `collect_persist_delta` until something else re-stamped it.
- **`orchestrators.rs` queued-prompt clear routes through `session_mut_by_index`** — `normalize_orchestrator_instances_with_persisted_non_running` now stamps the record before calling `clear_stopped_orchestrator_queued_prompts`, so the delete-session call path persists the cleared queued prompts to SQLite. The load-path caller re-persists identical rows on startup, which is a tiny waste but correct.

## Test-suite parallel execution flakiness: shared global env + temp paths

**Severity:** Medium - two tests were observed to fail intermittently during batched `cargo test --bin termal` runs in the April 16 session: `tests::acp_gemini::select_acp_auth_method_ignores_workspace_dotenv_credentials` (sometimes `::gemini_dotenv_env_pairs_ignore_workspace_env_files`) and `tests::shared_codex::shared_codex_thread_setup_persist_failure_does_not_tear_down_runtime`. Both passed when re-run in isolation. The pattern (pass-in-isolation, fail-in-batch) strongly suggests shared global state contamination — most likely process-wide env vars (`GEMINI_API_KEY`, `HOME`) and temp-dir paths that collide between parallel test threads.

**Current behavior:**
- Full-suite runs occasionally fail with one or two of the tests above; a re-run typically succeeds.
- The failures do not correspond to any real regression — the production code paths are correct.
- Undocumented flakes erode trust in the test suite and cost time whenever they surface in CI.

**Proposal:**
- Audit the flaky tests for process-wide state: look for `env::set_var` / `env::remove_var` / `TEST_HOME_ENV_MUTEX` usage that isn't serialized against other tests touching the same variables.
- Serialize the offending tests via a shared `Mutex` guard (either a hand-rolled `TEST_HOME_ENV_MUTEX` like the Gemini tests use, or `serial_test::serial`) if the root cause is env-var contention.
- For `shared_codex_thread_setup_persist_failure_does_not_tear_down_runtime` specifically: if the flake is temp-file path collision, switch to `tempfile::tempdir()` with unique per-test directories.
- Add a regression test harness that runs each flagged test 20 times back-to-back in the batch context to confirm the fix.

## Stale `src/tests.rs` can reappear in the working tree alongside `src/tests/mod.rs`

**Severity:** Low - during the April 16 session split of `src/tests.rs` into the `src/tests/` directory, a copy of the original pre-split file was observed reappearing in the working tree as a staged "new file" after later commits. The cause is not fully understood (possibly an IDE cache, a tool operation that restored an earlier snapshot, or accidental `git add` glob behavior). Rustc errors out with `E0761: file for module 'tests' found at both "src\tests.rs" and "src\tests\mod.rs"` under `cargo check --tests`, though `cargo test --bin termal` can sometimes still resolve the module to `src/tests/mod.rs` and pass, masking the problem.

**Current behavior:**
- The stale file reappeared at least once after the directory form was already committed; it was staged as `new file: src/tests.rs`.
- `cargo check --tests` fails immediately; `cargo test --bin termal` may or may not, depending on invocation path.

**Proposal:**
- Add a `.gitignore` or `.gitattributes` guard against a literal `src/tests.rs` path while the directory form is in use.
- Prefer running `cargo check --tests` (not just `cargo test`) in local verification, since the bin-only invocation can miss the ambiguity.
- If the reappearance was an IDE-side artifact, consider documenting the offending tool so future contributors avoid the same mistake.

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

## Delta persist drops tombstones on write failure

**Severity:** High - a single transient `persist_delta_via_cache` error can silently leak an orphan session row into SQLite. The deleted session's row never gets cleaned up and the session reappears on next restart.

`StateInner::collect_persist_delta` drains `removed_session_ids` via `std::mem::take` while holding the state mutex, before the persist thread calls `persist_delta_via_cache`. If the SQLite write then fails (locked DB, disk full, I/O error), the persist thread does not advance its watermark — so `changed_sessions` correctly retries on the next tick because the mutation stamp is still higher than the watermark. But `removed_session_ids` has already been taken out of `inner` and passed by value to the (now failed) write, and the session has already been removed from `inner.sessions`. There is no mutation stamp to retry from, and the tombstone vec was not pushed back into `inner` on error.

**Current behavior:**
- `collect_persist_delta` drains `removed_session_ids` into a local.
- Persist thread calls `persist_delta_via_cache`; on error, it keeps its watermark.
- On next tick, `collect_persist_delta` sees an empty `removed_session_ids` and no session with that id in `inner.sessions`.
- The orphan row stays in `sessions` table forever (or until another event coincidentally DELETEs it).

**Proposal:**
- On `persist_delta_via_cache` error, push the removed ids back into `inner.removed_session_ids` so the next tick retries them. Hold the lock again briefly to do it.
- Or: move the `mem::take` out of `collect_persist_delta` into the persist-thread caller, pass the ids by reference, and have the thread clear them from `inner` only on success.
- Add a regression test that injects an error into `persist_delta_via_cache` and asserts the tombstone survives across a retry.

## Remote sync rollback leaves stale session tombstones queued

**Severity:** High - a transient remote orchestrator-sync failure can persist deletes for sessions that were restored by rollback.

`sync_remote_state_inner` now uses `retain_sessions`, which records deleted session ids in `removed_session_ids` for delta persistence. If the later `sync_remote_orchestrators_inner` call fails, the rollback restores `sessions`, `next_session_number`, and `orchestrator_instances`, but it does not restore the previous tombstone accumulator. The next persist tick can therefore delete SQLite rows for sessions that are back in memory.

**Current behavior:**
- Remote session retention can queue tombstones in `inner.removed_session_ids`.
- The fallible orchestrator sync rollback restores session records but leaves the tombstones queued.
- A later delta persist can apply stale deletes to SQLite, making restored sessions disappear after restart.

**Proposal:**
- Include `removed_session_ids` in the rollback snapshot and restore it on failure.
- Or defer the retention/delete pass until after the fallible orchestrator sync has succeeded.
- Add a regression that forces orchestrator sync failure after remote session retention and asserts no stale tombstones remain queued.

## Imported discovered Codex threads lose their delta-persist stamp

**Severity:** Medium - newly discovered Codex thread sessions can be inserted in memory but skipped by SQLite delta persistence.

`import_discovered_codex_threads` creates a session, then performs a whole-record replacement with a local `record`. That local record still carries the construction-time `mutation_stamp: 0`, so the replacement can overwrite the stamped row that `create_session` inserted. Under the delta persist watermark, the row can then look unchanged and never be written to SQLite.

**Current behavior:**
- `create_session` pushes a stamped session row.
- `import_discovered_codex_threads` replaces that row with an unstamped local record.
- The background delta persist can skip the discovered thread row because its stamp is below the watermark.

**Proposal:**
- Re-stamp the slot after the whole-record replace, matching the create/fork paths.
- Prefer mutating the already-stamped inner record in place when practical.
- Add a production-delta persistence regression for importing a discovered Codex thread and verifying its row is written.

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

## `session_mut_by_index` leaks a mutation stamp on out-of-bounds miss

**Severity:** Medium - `next_mutation_stamp()` runs *before* `self.sessions.get_mut(index)` can fail. An out-of-bounds call silently advances `last_mutation_stamp` with no mutation to show for it. Divergent from `session_mut` (by id), which short-circuits correctly on the `find_session_index` miss.

At `src/state.rs:6565-6570`:

```rust
fn session_mut_by_index(&mut self, index: usize) -> Option<&mut SessionRecord> {
    let stamp = self.next_mutation_stamp();     // advances unconditionally
    let record = self.sessions.get_mut(index)?; // fails silently if OOB
    record.mutation_stamp = stamp;
    Some(record)
}
```

Callers that guard with `find_session_index` are safe, but the helper's own invariant is leaky: a typo or race between `find_session_index` and `session_mut_by_index` burns a stamp value that can never match any stored record.

**Current behavior:**
- Every OOB call to `session_mut_by_index` increments `last_mutation_stamp`.
- Nothing ties the burned stamp to any session, so the global watermark gap grows by one per miss.
- Not a correctness bug today (watermark math still works), but the invariant "stamp implies an actual mutation" is false.

**Proposal:**
- Invert the order: fetch `get_mut` first, then advance the stamp only inside the `Some` arm.
- Or explicitly document the leak as intentional (it isn't) and pin it with a test.
- Extend `state_inner_session_mut_helpers_stamp_the_record` with a miss case.


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

## Self-chained safety-net poll hard-cap can be missed

**Severity:** Medium - the 5-minute hard-cap on the post-send `/api/state` poll can fail to fire, letting the poll continue indefinitely under specific timing.

`ui/src/App.tsx`'s self-chained `setTimeout` poll clears `activePromptPollTimeoutIdRef.current` to `null` at the top of each fired callback and only re-populates it inside `scheduleNextActivePromptPoll` after the `await fetchState()` resolves. The hard-cap `setTimeout` calls `clearActivePromptPoll`, which clears whatever is currently in the ref. If the cap fires between "id cleared" (top of callback) and "id re-set" (next schedule queued), `clearActivePromptPoll` no-ops. The next `scheduleNextActivePromptPoll` call then queues a fresh 30 s timer that outlives the cap.

**Current behavior:**
- The cap timer clears the ref; if the ref is null, it's a no-op.
- Inside the chained callback, the ref is null while awaiting `fetchState()`.
- On large transcripts, `fetchState` can take multiple seconds.
- The cap can fire during that window and fail to stop the chain.

**Proposal:**
- Add a `capReached` flag (ref or closure-local) and have `scheduleNextActivePromptPoll` short-circuit when it is set.
- Alternatively record the deadline timestamp at start; the callback bails out when `Date.now() >= deadline`.

## `apply_remote_session_to_record` unconditionally clones the full transcript

**Severity:** Low - every remote-session hydration pays for a full-transcript `.clone()` even though the clone is used only in a rare preserve branch.

`apply_remote_session_to_record` starts with `let previous_messages = record.session.messages.clone();`. That clone is consumed only when `remote_session.messages_loaded == Some(false) && remote_session.messages.is_empty() && !previous_messages.is_empty()`. The common path (a real transcript update) clones and discards.

**Current behavior:**
- Every remote hydration (`get_remote_session`, create/fork remote proxy) clones the full transcript up front.
- The clone survives only if the preserve branch activates.
- Allocations and copy time scale with transcript size for every hydration, including the common case.

**Proposal:**
- Compute the branch condition first; use `std::mem::take(&mut record.session.messages)` only if the preserve branch will fire.
- Add a benchmark or log line capturing the avoided allocation on the common path.

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

## Safety-net poll uses `force + allowRevisionDowngrade` unconditionally

**Severity:** Low - every scheduled `/api/state` poll can overwrite SSE-advanced client state with an older server snapshot if the poll's response arrives after newer SSE events.

The chained poll calls `adoptState(freshState, { force: true, allowRevisionDowngrade: true })` regardless of whether a server restart actually happened. The flag exists to recover from server restarts that reset the persisted revision, but it is active for every poll — so a momentarily stale `/api/state` response landing after a newer SSE delta can roll the client back to the older snapshot. The poll is supposed to be a no-op when SSE is healthy.

**Current behavior:**
- Every poll adopts with `force + allowRevisionDowngrade`.
- SSE's revision ordering is overridden on the poll path.
- In practice the race window is small, but it can cause transient UI rollback.

**Proposal:**
- Gate `allowRevisionDowngrade: true` behind an explicit "server restart detected" signal (e.g., the state carries a freshly reset instance id).
- For the steady-state poll, use `force: false` and trust SSE.

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

## Implementation Tasks

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
- [ ] P1: Add remote-sync rollback tombstone coverage:
  force `sync_remote_orchestrators_inner` to fail after remote-session
  retention records deletes, then assert the rollback restores
  `removed_session_ids` along with sessions and orchestrator instances.
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
- [ ] P2: Extend `state_inner_session_mut_helpers_stamp_the_record` with
  a negative case: assert `inner.session_mut("does-not-exist")` returns
  `None` and that `inner.last_mutation_stamp` either advances (and is
  documented as such) or stays unchanged on the miss. The two helpers
  currently diverge — `session_mut` (by id) short-circuits on
  `find_session_index`, while `session_mut_by_index` increments the
  counter before `get_mut` can fail. Pin whichever semantics the tree
  commits to.
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
