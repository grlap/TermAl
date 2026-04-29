# Bugs & Known Issues

This file tracks reproduced, current issues. Resolved work, speculative refactors,
cleanup notes, and external limitations do not belong here. Review follow-up task
items live in the Implementation Tasks section.

Also fixed in the current tree: preferences fallback recovery now only trusts replacement `serverInstanceId` snapshots after backend-unavailable evidence, and preferences are no longer synced before `adoptState` accepts the fallback. The preferences regression test now uses lazy deferreds, drives the async recovery path explicitly, suppresses only the known detached-handler React warning, and asserts a recovered session that can only appear through successful state adoption.

Also fixed in the current tree: snapshot-bearing action responses now route rejected snapshots through authoritative action recovery with `allowUnknownServerInstance: true`, and resume-triggered recovery now opts into replacement-instance adoption. Focused visibility tests cover both a replacement-instance approval response and a resume-triggered replacement `/api/state` snapshot.

Also fixed in the current tree: fork-thread notice coverage now asserts unmaterialized stale fork responses do not render phantom notices, while a direct materialized fork response still shows the intended fork notice.

Also fixed in the current tree: reconnect fallback polling now stays armed across replacement-instance fallback adoption until `EventSource.onopen` or a confirmed live event proves the SSE stream reopened (the revision-equality rearm guard now remains for same-instance probes while replacement-instance adoption gets a narrower rearm path). A fake-timer regression test asserts the second recovery poll fires after replacement-instance adoption when SSE has not reopened. The hydration adoption switch keeps its silent retry fallback for impossible outcomes, avoiding user-facing assertion text while retaining TypeScript exhaustiveness. Direct unit coverage was added for `hydrationSessionMetadataIsAhead` covering the equal-counts/newer-mutation-stamp branch and the missing-count fallback. The `requestActionRecoveryResyncRef` offline-preserve reorder now carries an inline comment naming the load-bearing invariant.

Also fixed in the current tree: connection-retry notice liveness now derives from session lifecycle and retry sequencing instead of just "is this the latest assistant message". A new `ConnectionRetryDisplayState` union (`live` / `resolved` / `superseded` / `inactive`) drives the spinner, heading, and aria-live state via a per-message map computed in `SessionPaneView`. Two new MessageCard tests cover the `superseded` and `inactive` rendering paths.

Also fixed in the current tree: the inline-zone id for Mermaid fences in Markdown files no longer depends on absolute line numbers, so editing text above a fence keeps the same id and preserves the diagram iframe DOM across keystrokes. The id is now `mermaid:${sameBodyOrdinal}:${quickHash(body)}`, with a same-body ordinal as a tiebreaker for collisions. New unit tests cover line-shift stability and same-body ordinal disambiguation; the `SourcePanel.test.tsx` "latent stability gap" assertion is flipped accordingly.

Also fixed in the current tree: the persist worker now retries `persist_delta_via_cache` failures with capped exponential backoff through `recv_timeout`, preserving tombstones and changed-session deltas across transient SQLite errors while still observing shutdown during retry waits. The SQLite database file now gets `0o600` permissions on Unix via a new `harden_local_state_file_permissions` helper, applied immediately after open/commit. Windows relies on `~/.termal/` ACL inheritance.

Also fixed in the current tree: SQLite WAL/SHM sidecars are now hardened alongside `termal.sqlite` on Unix after WAL setup, schema initialization, and successful write commits. Connection-retry card copy and retry-state classification now live in shared helpers, so transcript search indexes the rendered `live` / `resolved` / `superseded` / `inactive` notice text instead of stale alternate states. Settled retry notices now share the muted non-live card styling.

Also fixed in the current tree: the lazy hydration effect now guards against repeated `requestActionRecoveryResyncRef` calls when `response.session.id !== sessionId` via a new `hydrationMismatchSessionIdsRef` "tried-once" gate, breaking the transient mismatch — resync — refetch — mismatch loop. The `sessionCreated` reducer's `existingIndex !== -1` branch is now routed through `reconcileSessions` so memoized `MessageCard` children keep stable identity when SSE materializes a session before the API response lands.

Also fixed in the current tree: `adoptActionState` now schedules authoritative action recovery for unrecoverable adoption failures, but treats stale same-instance action snapshots as UI success because a newer local revision already includes that server-side mutation. Existing caller guards still stop success-side cleanup for replacement/unknown failures. `handleCancelQueuedPrompt` keeps its catch-path state fetch as a passive best-effort `adoptState` refresh, so failed requests are not classified as action success.

Also fixed in the current tree: retry-display lookup no longer invalidates `renderSessionMessageCard` on every streaming chunk; `ConnectionRetryCard` now gets copy, classes, spinner, and aria state from one shared presentation helper; hydration mismatch suppression is pruned with unhydrated sessions and has inline invariants; created-session adoption reconciles both append and replace paths; reconnect fallback rearm now documents why replacement-instance recovery cannot rely solely on revision equality. SQLite hardening now tightens the TermAl data directory to `0700` before DB opens, treats chmod/metadata hardening failures as warnings rather than persistence blockers, and the persist worker retry loop uses capped exponential backoff with `recv_timeout` so shutdown is observed during retry waits.

Also fixed in the current tree: retry-notice search now indexes the rendered lifecycle copy and the literal stored retry notice, so settled cards still match "Retrying automatically" while state-aware searches still find recovered/superseded/live copy. The thin `connectionRetryNoticeCopy` wrapper and bare `"Connection"` search token are gone, the retry-state classifier now documents its precedence contract, and the interleaved `retry -> response -> retry` search case is covered. `SessionPaneView` now documents the render-time retry-map ref invariant across assistant appends and lifecycle transitions. The inactive retry-card test now asserts the body detail. Persisted sessions now scrub runtime-only `session_mutation_stamp` on save and load, with a regression proving `sessionMutationStamp` is absent from serialized session records and manually injected stamps are cleared on load. `isStaleSameInstanceSnapshot` now documents its monotonic-revision invariant and covers null/undefined instance-id edge cases.

Also fixed in the current tree: test-only legacy JSON state loading now rejects files larger than 100 MiB before reading them into memory, and the regression uses a small size cap instead of allocating a 100 MiB sparse fixture on Windows. Production SQLite state directory creation now uses a Unix `DirBuilder` mode of `0700` before DB open, then keeps the existing best-effort hardening pass. The disconnected create-session persistence fallback now writes a full persisted snapshot instead of only the created row, so sibling create-flow mutations follow the same durability shape as the normal background persist path. SQLite sidecar hardening now also covers `-journal`, and the reconnect fallback polling test derives its backoff waits from the exported reconnect delay constant.

Also fixed in the current tree: session-scoped stale same-instance action responses now clear the hydration mismatch suppression entry for the affected session before reporting action success, so the "tried-once" hydration mismatch gate has the same cleanup behavior as accepted authoritative action snapshots.

Also fixed in the current tree: `shared_codex_thread_setup_persist_failure_does_not_tear_down_runtime` no longer relies on a fixed 50 ms sleep before asserting the background thread setup failure landed. It now polls up to a bounded deadline, fails immediately if `StartTurnAfterSetup` is queued, and waits for the expected in-memory Error state before checking runtime retention.

Also fixed in the current tree: oversized test-only legacy JSON fixtures now name the `TERMAL_LEGACY_STATE_MAX_BYTES` escape hatch in the load error, and the fixture load path reads that env-var override while preserving the 100 MiB default cap. Tests pin the cap and the override parser. `AdoptActionStateOptions` now drops only the hydration-mismatch bookkeeping field via rest destructuring before forwarding adoption options, the global project-create adoption path has an explanatory comment, and the retry-map render-time ref comment now names the complete-message streaming assumption.

Also fixed in the current tree: `MessageCard.test.tsx` now explicitly covers `connectionRetryDisplayState="resolved"`, the stale same-instance snapshot unit coverage is split into focused cases, same-body Mermaid region tests pin ordinal ids `[0, 1]`, the runtime mutation-stamp persistence test is split into save/load assertions, the `App.session-lifecycle.test.tsx` indentation regression is corrected, the defensive `adoptActionState` mount guard now documents that callers already check mount synchronously, and `app-session-actions.test.ts` pins that session-scoped stale same-instance action success clears exactly the acted session id without scheduling recovery.

Also fixed in the current tree: hydration-only `AdoptActionStateOptions` now normalize to `undefined` before calling `adoptState`, so `app-session-actions.test.ts` matches the implementation. The reconnect fallback polling regression test now keeps harness setup on real timers and switches to fake timers only around the reconnect backoff window, restoring real timers before harness teardown.

Also fixed in the current tree: action-state adoption now returns an explicit outcome (`adopted` / `stale-success` / `recovering` / `unmounted`), and the settings/model-refresh callers read from `sessionsRef.current` after stale same-instance success instead of the stale response body. Session-scoped action recovery now threads the acted session id as recovery-only navigation context without forcing ordinary successful adoption to open that session. `app-session-actions.test.ts` now covers stale same-instance action success and recovery context. The test-only legacy JSON max-bytes override emits a one-shot stderr warning, invalid override inputs now have regression coverage, the oversized fixture error uses named captures for the single measured byte count, and resolved/superseded connection-retry card tests now assert `aria-live` plus settled/resolved class names.

Also fixed in the current tree: reconnect fallback polling now has the converse replacement-instance test proving a data-bearing SSE `state` event after `EventSource.onopen` disarms subsequent polls, while bare `onopen` does not. `SessionPaneView` narrows retry-display map memoization to the classifier inputs (`activeSession?.messages` and `activeSession?.status`), and the duplicated exhaustive-switch helpers now use a shared `ui/src/exhaustive.ts` `assertNever`.

Also fixed in the current tree: reconnect rearm-on-success keeps the original same-instance revision-parity guard for one-shot probes while timer-driven reconnect fallback requests carry an explicit "rearm until live event" option. The replacement-instance fake-timer tests use a timing margin around the rearmed backoff boundary to account for `settleAsyncUi`'s fake-timer drain. Later reconnect fixes split automatic same-instance progress from manual-retry live-stream proof, so `/api/state` catch-up no longer masquerades as SSE recovery.

Also fixed in the current tree: action recovery options no longer inherit adoption-side `openSessionId`/`paneId`; only `recoveryOpenSessionId`/`recoveryPaneId` can set recovery navigation. Retry-display renderer invalidation now includes a retry-state signature so classification changes propagate without tying the renderer to every map rebuild. The defensive `"unmounted"` action outcome is documented as caller-equivalent to no-success, `app-session-actions.test.ts` covers the adopted branch reading from response sessions, and `ui/src/exhaustive.ts` has the required split-file provenance header.

Also fixed in the current tree: the erroneous `clearReconnectStateResyncTimeoutAfterConfirmedReopen()` call was removed from `EventSource.onopen`, and the helper now carries an inline warning that it is only for data-bearing SSE handlers. The round-9 reconnect test dispatches live state after `dispatchOpen()` so the disarm path is exercised by confirmed data rather than by the socket handshake alone.

Also fixed in the current tree: production persistence now resolves directly to `~/.termal/termal.sqlite` and `load_state` no longer imports or renames legacy `sessions.json`; the old JSON load helpers are compiled only for tests/fixtures. README, architecture docs, and the persistence comments now describe SQLite as the sole runtime session store.

Also fixed in the current tree: connection-retry parsing, display-state classification, and presentation metadata now live in `ui/src/connection-retry.ts` with a split-file provenance header. `app-utils.ts` no longer owns that subsystem, and MessageCard, transcript search, SessionPaneView, and MarkdownContent tests import the dedicated module directly.

Also fixed in the current tree: SQLite permission hardening re-runs after every cached delta persist commit, matching the direct full-snapshot path and covering WAL/SHM/journal sidecars that can be recreated after earlier hardening.

Also fixed in the current tree: the `SessionBody` comparator comment now records the accepted streaming tradeoff: in session view the message renderer intentionally tracks streaming flags, so active streaming chunks can re-render `SessionBody` while `SessionConversationPage` still defers the visible message list before heavy content rendering.

Also fixed in the current tree: `renderSessionPromptSettings` now documents the accepted callback-dependency tradeoff. SessionPaneView keeps one renderer for the four agent prompt cards, and Codex-only inputs can rebuild the callback for non-Codex agents only on explicit settings/thread actions.

Also fixed in the current tree: `AgentSessionPanel.test.tsx` now covers the `SessionBody` comparator branches for `viewMode="commands"` and `viewMode="diffs"`, proving renderer-only parent rerenders refresh the visible command and diff cards.

Also fixed in the current tree: `hydrate_remote_session_target` now routes broad-state commit-before-rejection through `commit_applied_remote_state_before_rejection`, preserving fetched remote state before stale/missing proxy errors without repeating the same commit/error boilerplate at each early return.

Also fixed in the current tree: `prepare_termal_gemini_system_settings_writes_override_file` now holds `TEST_HOME_ENV_MUTEX` and redirects `USERPROFILE`/`HOME` to its own temp home before preparing the Windows override, removing the full-suite race with sibling Gemini env tests.

Also fixed in the current tree: `live-updates.test.ts` now pins referential identity for the `sessionCreated` existing-session reconciliation path, asserting both the retained message array and unchanged message object survive summary replacement. This covers the memoized-card preservation invariant behind the `reconcileSessions` branch.

Also fixed in the current tree: targeted remote delta hydration now treats an upstream `messages_loaded: false` session response as "no full repair available" and falls through to the narrow delta apply instead of hard-erroring. A remote regression covers the chained-summary case by returning a metadata-only session from `/api/sessions/{id}` and asserting the incoming delta still materializes the proxy transcript.

Also fixed in the current tree: App-level live-delta fixtures now include required `messageCount` on session-scoped `messageCreated` and `textReplace` events across reconnect, visibility, orchestration, backend-connection, and live-state delta tests. A scan over `dispatchNamedEvent("delta", ...)` / `dispatchDelta(...)` fixtures now finds no current session-scoped delta test payload missing `messageCount`.

Also fixed in the current tree: unloaded remote-proxy `get_session()` hydration now uses a bounded visible-pane fetch timeout and falls back to the cached local summary when the owner cannot provide a full transcript, returns metadata-only data, or loses a freshness race with a newer remote state snapshot. Remote regressions cover both metadata-only owner responses and stale transcript rejection after a side `/api/state` sync.

Also fixed in the current tree: the remote orchestrator summary-preservation regression now validates every session in the republished `OrchestratorsUpdated.sessions` payload. The fixture includes both metadata-only and hydrated incoming remote sessions, asserts all localized output sessions are metadata-first, and checks the full multiset of preserved `message_count` values.

Also fixed in the current tree: the backend reconnect fallback test for an applied pre-reopen session delta flushes the rAF-backed render, then proves both the streaming preview and active-pane message body appear while the reconnect badge remains active and before the fallback `/api/state` request fires.

Also fixed in the current tree: targeted session hydration now treats `messagesLoaded: false` `SessionResponse` payloads as still-unhydrated instead of forcing them loaded, and the architecture docs describe the metadata-only fallback contract. Remote hydration fallback paths now log the fallback cause and the helper documents its lock and retry contract. Connection-retry notice classification now uses the nearest later assistant item, with search-index coverage for `retry -> retry -> final output` proving the older retry is superseded while only the newer retry resolves.

Also fixed in the current tree: `REMOTE_VISIBLE_SESSION_HYDRATION_TIMEOUT` now lives in `state_accessors.rs` next to the `get_session()` fallback path that consumes it, instead of relying on the `include!`-flattened namespace from `remote_routes.rs`.

Also fixed in the current tree: `adoptActionState` now explicitly constructs the `AdoptStateOptions` object from only `openSessionId` and `paneId`, so future action-only option keys cannot silently pass through to `adoptState`.

Also fixed in the current tree: `app-live-state.test.ts` now covers the `hydrationMismatchSessionIdsRef` tried-once gate directly. The hook-level tests prove repeated mismatched `fetchSession` responses only invoke action recovery once, and that an authoritative `adoptState` call clears the gate so a later mismatch can request recovery again.

Also fixed in the current tree: `SessionPaneView.retry-display.test.tsx` now opens the Sessions surface and clicks the session row before asserting retry card headings, and it covers the resolved branch with a retry followed by ordinary assistant output. The backend reconnect delta test also opens the session pane before asserting streamed message body text. Timer-driven reconnect fallback requests keep replacement-instance polling armed until a confirmed live SSE event arrives.

Also fixed in the current tree: the orchestrator restart persistence tests now disable the background persist channel immediately after constructing their test `AppState`, forcing setup commits through the synchronous test fallback before the tests read the JSON fixture on restart.

Also fixed in the current tree: the persist worker retry bookkeeping now has direct Rust coverage. `src/tests/persist.rs` pins the capped-backoff state transition, timeout-driven retry tick, Delta wake during backoff, and disconnected-channel shutdown branch around `recv_timeout`.

Also fixed in the current tree: state-resync request options now coalesce into one pending option bag instead of ten parallel refs. Boolean flags retain the strongest requested semantics, explicit navigation targets overwrite the prior pending target, and `app-live-state-resync-options.test.ts` pins empty defaults, flag preservation, navigation target retention, and consume-and-clear behavior.

Also fixed in the current tree: `adoptFetchedSession` now delegates its four-outcome decision logic to a pure `classifyFetchedSessionAdoption` helper. Direct unit coverage pins `adopted`, `restartResync`, `stateResync`, and `stale` classifications.

Also fixed in the current tree: visible remote-session hydration fallback is narrowed to explicit recoverable misses (metadata-only transcript, remote connectivity failure, or documented freshness races) and no longer masks local lookup/persistence or bad remote protocol errors. A remote regression proves an owner response with a mismatched session id returns `BAD_GATEWAY` instead of cached-summary success. Connection-retry classification now treats a later user prompt as a turn boundary so old retry notices settle while a new active turn runs, and transcript search covers that state. Persist retry restores only drained explicit tombstones after a failed delta write; hidden-session deletes are regenerated from still-hidden records instead of being duplicated in the retry queue.

Also fixed in the current tree: `app-live-state.test.ts` now makes `makeCountingActionRecoveryRef` return a stable `.current` wrapper until the ref setter receives a new callback, matching production ref identity behavior. The direct helper-only assertion was removed; hook-level hydration recovery tests now exercise the behavior through `useAppLiveState`.

Also fixed in the current tree: the pure state-resync option helpers moved from `app-live-state.ts` into `app-live-state-resync-options.ts`, and the session-hydration adoption classifier family moved into `session-hydration-adoption.ts`. Existing direct tests now import those split modules while `useAppLiveState` keeps only the side-effect orchestration.

Also fixed in the current tree: `src/persist.rs` now documents that the legacy JSON byte cap and `TERMAL_LEGACY_STATE_MAX_BYTES` override are test-fixture safety for fixture/migration coverage, not production runtime import guardrails.

Also fixed in the current tree: `SqlitePersistConnectionCache` no longer short-circuits state-file hardening after the first successful cached delta commit. The cached path now re-runs the existing best-effort SQLite main/sidecar hardening pass after every successful commit, so recreated WAL/SHM/journal sidecars are covered without waiting for cache invalidation.

Also fixed in the current tree: the remote targeted-hydration "summary returned instead of full transcript" path now carries a typed `ApiErrorKind::RemoteSessionMissingFullTranscript` tag. The delta hydration fallback checks that tag instead of string-matching `err.message`, so message copy changes cannot widen or break the recoverable-summary branch.

Also fixed in the current tree: `SessionPaneView`'s AgentSessionPanel render-callback cluster moved into `SessionPaneView.render-callbacks.tsx`. The pane component now imports `useSessionRenderCallbacks`, keeping the four stable card/prompt renderers out of the large pane orchestration file without changing their dependencies.

Also fixed in the current tree: backend connection tests now clear localStorage between cases; hydration adoption's impossible default arm falls back to silent retry instead of surfacing an assertion toast; `SessionPaneView` no longer writes the retry-display map ref during render; visible remote hydration fallback uses the typed `RemoteSessionMissingFullTranscript` kind; Unix SQLite state hardening now returns verified errors unless `TERMAL_ALLOW_INSECURE_STATE_PERMISSIONS` explicitly opts into insecure best-effort mode; and retry-display integration coverage now pins the active-session later-user-prompt transition that changes only the retry classification signature.

Also fixed in the current tree: `connection-retry.test.ts` now covers retry notice parsing and `connectionRetryPresentationFor` across `live`, `resolved`, `superseded`, and `inactive` display states, including resolved copy with and without an explicit attempt label.

Also fixed in the current tree: stale same-instance action responses now require target-specific evidence before reporting UI success. Session-scoped actions require the current session's `sessionMutationStamp` to be at least as new as the response session's stamp, deletion-style responses require the target session to already be absent locally, and project creation requires the created project id to already exist locally; otherwise the action schedules authoritative recovery.

Also fixed in the current tree: `app-live-state.test.ts` now covers the hook-level hydration adoption side effects for `stateResync`, `restartResync`, offline restart-preserve retry, and stale hydration retry paths, so the switch that consumes `classifyFetchedSessionAdoption` outcomes is pinned beyond the pure classifier tests.

Also fixed in the current tree: `connection-retry.test.ts` and `app-live-state-resync-options.test.ts` now provide direct unit coverage for the round-11 extractions. The connection-retry tests cover retry notice parsing and `connectionRetryPresentationFor` across `live` / `resolved` / `superseded` / `inactive` display states; the resync-options tests pin empty defaults, monotonic flag preservation, reconnect rearm preservation across coalesced requests, navigation-target retention, and consume-and-clear behavior.

Also fixed in the current tree: stale same-instance session-settings success now preserves the pre-optimistic live session as target evidence, the dead `assertNever` import in `app-live-state.ts` is gone, and project-scoped stale create-project success has direct `useAppSessionActions` coverage. `rearmUntilLiveEventOnSuccess` is now OR-coalesced like the other strongest-semantics resync flags.

Also fixed in the current tree: stale action adoption classification now lives in `ui/src/action-state-adoption.ts`, with direct tests for same-instance stale success, missing target evidence, preserved pre-optimistic session evidence, deletion-style absence evidence, and project-create evidence. `useAppSessionActions` now keeps the UI cleanup/recovery side effects while delegating the pure rejected-snapshot decision.

Also fixed in the current tree: visible remote-session hydration fallback now branches on typed `ApiErrorKind` values for remote connection failures, freshness races, and missing full transcripts instead of parsing user-facing error text. Direct tests prove copy-only matching no longer controls recoverability. Local `ssh` spawn failures are now tagged as `RemoteConnectionUnavailable`, so they use the same recoverable cached-summary path as remote connectivity failures.

Also fixed in the current tree: Unix SQLite state-permission hardening helpers are now test-visible on Unix, with direct `#[cfg(unix)]` tests for owner-only file mode, owner-only directory mode, SQLite `-wal`/`-shm`/`-journal` sidecar coverage, and the explicit insecure-permission override path. The tests compile out on Windows.

Also fixed in the current tree: visible-session hydration retry timers now restart hydration for the specific stale session instead of bumping a global retry counter that reran the whole visible-target effect. `app-live-state.test.ts` covers a two-session retry where only the stale session is fetched again.

Also fixed in the current tree: session-scoped action recovery now derives the recovery pane from the workspace tab that owns the acted session, so rejected/stale action snapshots recover into the originating pane instead of the currently active pane. The non-backend preferences fallback test now asserts the current-instance session remains present while rejecting the replacement snapshot. Unix state-file hardening now uses `symlink_metadata` and skips symlink sidecars, cached post-commit hardening now runs after the SQLite commit is durable and is enforced through the post-commit helper, the persist retry seed constant is named for its actual role, the hydration-mismatch clear API documents its cross-module invariant, retry-display tests pin the settled/inactive and lifecycle-removal UI contracts, SourcePanel stabilizes rendered-Markdown prop callbacks while reusing one normalized editor buffer, and `AdoptActionStateOutcome` now collapses the previously indistinguishable recovery/unmounted branches into one `deferred` no-success outcome. Reconnect fallback tests now derive their pre-deadline waits from `RECONNECT_STATE_RESYNC_DELAY_MS` and use a wider boundary margin for the second backoff probe. The rendered connection-retry search coverage is split into named cases for live, inactive, resolved, superseded, retry-then-resolved, new-prompt, and interleaved sequences. `useAppDragResize` now has direct handler coverage for pane drag start/end, launcher drag start/end, tab drops, and split pointer resizing. `session-store.ts` now exposes a direct `removeSessionFromStore` eviction path, and pending incremental session-store flushes prune ids that disappear before the render frame.

Also fixed in the current tree: action-recovery resync explicitly opts out of live-event rearming, and the session hydration endpoint table now documents `SessionResponse { revision, serverInstanceId, session }` plus recoverable remote-proxy `messagesLoaded: false` success.

Also fixed in the current tree: `AdoptActionStateOptions` now documents the adoption-navigation versus recovery-navigation option split, project-scoped stale create-project handling has both classifier and hook negative-path coverage, and git diff enrichment tests now pin that recoverable remote hydration error kinds do not emit Markdown degradation notes. `PersistDelta` now names drained explicit tombstones separately from the full removed-id write set, the persist worker restores them through `StateInner::restore_drained_explicit_tombstones`, and a regression proves synthesized hidden-session deletes are regenerated rather than requeued as explicit tombstones. The repeated SQLite hardening passes after schema initialization now carry comments naming the deliberate defense-in-depth against sidecars created after connection open. Cached delta persistence now reruns fatal database/sidecar redirection preflight before cached writes and enforces post-commit chmod/mode hardening after rows are durable. Unix SQLite state-file hardening now rejects symlinked main or sidecar paths and chmods/verifies regular files through an `O_NOFOLLOW` file handle to close the residual path-swap window. The local SSH spawn-error mapping now has direct coverage proving it returns the sanitized OpenSSH/PATH message with `RemoteConnectionUnavailable`. The App-level online/offline reconnect test now proves the browser `online` listener triggers immediate reconnect fetches and still adopts replacement-instance state after a backend restart.

Also fixed in the current tree: math fence ids still deliberately include line positions, but the source now documents why that asymmetry is acceptable compared with Mermaid iframe remounts.

Also fixed in the current tree: same-instance reconnect fallback adoption no longer masquerades as live SSE recovery. Automatic reconnect probes stop self-rearming after a same-instance `/api/state` snapshot strictly advances the revision, which resets watchdog baselines without creating an endless polling loop. Manual reconnect retry now preserves its "prove data-bearing SSE after same-instance progress" policy across stale-snapshot reschedules, with comments on the reconnect scheduler option. The non-backend preferences fallback regression now seeds the current-instance session before asserting the replacement snapshot was rejected. SQLite state-file symlink rejection now runs before SQLite opens/configures the database path, covers known sidecars, and cannot be downgraded by `TERMAL_ALLOW_INSECURE_STATE_PERMISSIONS`; Windows builds now reject reparse-point state directories, database files, and sidecars before opening SQLite, with the parent-directory check owned by the Windows directory-hardening hook.

Also fixed in the current tree: failed queued-prompt cancel fallback now has `useAppSessionActions` coverage proving the passive `adoptState` refresh, no action-recovery resync, and original error reporting. Deferred session-store pruning now has hook-level coverage for queued delta ids that disappear before the pending render frame flushes. Connection-retry display state now keeps a content-stable map identity and the renderer callback depends on the actual map instead of a signature proxy. The duplicate Mermaid body ordinal edge case is documented as an accepted trade-off. Recoverable visible-session hydration errors now document their visible-pane/delta-replay semantics and BAD_GATEWAY kind contract, fallback logging records both the recoverable cause and any secondary fallback failure, and `ApiErrorKind` is documented as in-process-only metadata. Persist retry waiting now matches `PersistRequest` explicitly, SQLite cache replacement validates the new connection before replacing the old one, post-commit hardening is a path-keyed helper, and Unix directory hardening rejects symlinked state directories. The app-live-state test harness no longer uses the synthetic `hydrationRun` prop.

Also fixed in the current tree: backend reconnect tests now replace the remaining raw reconnect-fallback timer literals with delay/max-delay-derived constants, and the canonical first-attempt pre-deadline constant is now `RECONNECT_STATE_RESYNC_DELAY_MS - 1`. The stale app-live-state offline-restart entry was removed because the hook test is intentionally named as a manual retry, while App-level browser-online reconnect behavior is already covered. Cached SQLite delta persistence now enforces post-commit chmod/mode hardening after the rows are durable, while cached writes still repeat fatal redirection preflight before each transaction. `connection_for` no longer clears the cached path/connection immediately before overwriting both fields. Recoverable visible-session hydration fallback now surfaces a local `404 session not found` when the cached-summary fallback discovers the session was deleted, while preserving the original recoverable remote error for transient fallback failures. `ApiErrorKind` now documents its in-process-only/non-serialized contract at the enum definition, and `docs/architecture.md` records that typed recovery metadata does not survive forwarded remote HTTP error responses. The shared remote-hydration recovery predicate is now named `is_recoverable_remote_hydration_miss`, documents both visible-pane and delta-replay semantics, and keys recoverability on the typed kind instead of coupling it to `BAD_GATEWAY` status.

Also fixed in the current tree: visible remote-session hydration recoverability now keys on the typed `ApiErrorKind` alone, no longer gates on `BAD_GATEWAY` status. The renamed `is_recoverable_remote_hydration_miss` documents both visible-pane and delta-replay consumers. `get_session_hydration_fallback_response` now tags the local cached-summary miss as `LocalSessionMissing`, and `select_visible_session_hydration_fallback_error` selects that typed local miss over the original recoverable remote error while preserving the remote error for untyped or transient fallback failures. `get_session` delegates fallback logging and error selection to `recover_visible_session_hydration`, and the positive recoverability test now enumerates multiple statuses instead of only `bad_gateway`. `ApiErrorKind`'s in-process-only / non-serialized contract is stated at the enum-level docstring, and `docs/architecture.md` documents that error responses carry only `{ error: string }` and that typed kinds do not survive an HTTP hop. `backend-connection.test.tsx` migrated ~20 hardcoded reconnect-fallback timer literals to named constants (`RECONNECT_STATE_RESYNC_DELAY_MS`, `_SECOND/_FOURTH/_EIGHTH/_MAX_PRE_DEADLINE_MS`).

Also fixed in the current tree: cached SQLite post-commit hardening now routes through the shared hardening helper after durable deltas. `cargo check --tests` confirms the non-test cached persist path gating remains consistent. The git-diff Markdown enrichment helper now documents why non-git `ApiErrorKind` values return `None` in the defensive arm. The existing `docs/mermaid-demo.md` edit is retained and documented as an intentional Mermaid/Markdown demo smoke case, so it is no longer treated as incidental.

Also fixed in the current tree: Scenario B backend reconnect tests now use a dedicated `RECONNECT_STATE_RESYNC_PRE_DEADLINE_FROM_ERROR_MS` derived from the 100ms elapsed time between `dispatchError()` and the deadline assertion, while the canonical pre-deadline constant remains `RECONNECT_STATE_RESYNC_DELAY_MS - 1` for fresh timers. The constants block documents the two timing contracts. Cached SQLite writes now preflight both parent-directory redirection and database/sidecar redirection before every transaction, keep post-commit symlink/reparse detection and chmod/mode verification fatal after a durable commit. The Windows reparse-point helper now uses the conventional doc-comment/`#[cfg]` order. The stale same-instance reconnect reschedule now documents how it carries the manual-retry live-SSE-proof flag. The git-diff enrichment suppression fixture now includes `LocalSessionMissing`, and the visible-session hydration fallback selector documents why a typed local miss supersedes a recoverable remote hydration miss.

Also fixed in the current tree: the `Ok(None)` visible-session fallback path now documents why it deliberately returns the cached summary directly: there is no upstream remote error to preserve, and any vanished local record is tagged as `LocalSessionMissing`. `get_session_hydration_fallback_response` has direct typed-miss coverage. Action-recovery resync now explicitly sets `rearmOnSuccess: false`, and the existing offline restart-recovery test pins that pending replacement-instance adoption intent survives an offline observation. `MarkdownContent.test.tsx` clears the connection-retry spy in `beforeEach`, hydration retry tests use the exported first retry-delay constant with comments, `isStaleSameInstanceSnapshot` documents persisted mutation-stamp scrubbing, `action-state-adoption.ts` names its `state-revision.ts` invariant dependency, `restore_drained_explicit_tombstones` now dedups through a `HashSet` instead of repeated `Vec::contains` scans, and `source-renderers.test.ts` pins the accepted same-body Mermaid ordinal remount trade-off.

Also fixed in the current tree: bad reopened-SSE recovery now uses a cause-specific `pendingBadLiveEventRecovery` flag set only by parse/reducer failure catch arms, so watchdog wake-gap reconnect probes can stop after same-instance snapshot progress while bad live-event recovery keeps polling until a healthy data-bearing SSE event confirms the stream. The state-setting wrapper was removed. Cached SQLite path swaps no longer carry stale post-commit warning state, cached writes recheck path redirection before each transaction, post-commit redirection errors retain the original hardening context, and the Scenario B reconnect tests use the same elapsed-time constant in both deadline math and timer advancement. Visible-session hydration now documents the recovery helper contract, tags primary and fallback local session misses consistently, and names the selector policy. The architecture docs now call out that typed `ApiErrorKind` recovery is single-hop in chained remote topologies. The hydration retry delay export has a test-timing doc comment, Mermaid ordinal tests pin both the stable and shifting ids, the offline/online reconnect mock returns the recovered session it asserts, and `docs/mermaid-demo.md` describes its bare Mermaid-like smoke case.

Also fixed in the current tree: non-cached full-state SQLite writes now share the cached path's post-commit hardening semantics. Redirection/symlink failures remain fatal after commit, but chmod/metadata failures warn after durable rows are written instead of making behavior depend on whether the full-state or cached-delta path ran first. Reconnect coverage now pins both pending bad reopened-SSE recovery polling and the manual stale-then-fresh same-instance retry path continuing to poll until a data-bearing SSE event confirms recovery.

Also fixed in the current tree: post-commit SQLite redirection validation now runs after every durable commit, even when chmod/mode hardening succeeds or is a no-op, while preserving chmod failure context if a later redirection check also fails. Reconnect fallback no longer force-adopts lower same-instance `/api/state` snapshots; only confirmed replacement-instance recovery can downgrade revisions. Stale-but-valid ignored SSE frames no longer clear bad live-event recovery while `pendingBadLiveEventRecovery` is active. The recovery flag and action-recovery rearm call now document their contracts inline, visible-session lookup tags mid-flight local 404s consistently, explicit tombstone restoration avoids cloning the existing removed-id set, and reconnect tests use grouped timer constants plus a named inter-event gap.

Also fixed in the current tree: cached SQLite writes now keep fatal per-transaction redirection checks without repeating parent-directory chmod/metadata hardening on every cached delta tick; directory hardening remains on connection open. The reconnect manual-retry carry-forward predicate is now named before being passed into the scheduler. Bad reopened-SSE recovery coverage now forces same-instance forward progress and proves polling still rearms, so the `pendingBadLiveEventRecovery` branch is directly pinned. Still-current coverage gaps remain as normal active sections.

Also fixed in the current tree: local visible-session lookup misses are tagged with `LocalSessionMissing` at the producer instead of being reconstructed from a `"session not found"` string at the consumer. Reconnect fallback now uses a shared authoritative-snapshot base predicate, still allowing lower revisions only for confirmed replacement instances. Bad-live-event recovery clears stale confirmation state when malformed state/delta payloads arrive, workspace file hints no longer confirm that recovery, current-revision ignored deltas can still prove the reopened stream is healthy, and an applied `messageCreated` delta after malformed state payload recovery now has direct coverage proving it clears polling. Post-commit SQLite permission hardening is fatal again unless `TERMAL_ALLOW_INSECURE_STATE_PERMISSIONS` explicitly converts the chmod/mode failure to success. The backend restart and stale-same-instance reconnect tests now distinguish replacement instance ids and stale UI text, and small doc/test cleanup landed for typed remote error reconstruction, git-diff error-kind fallthrough, temp-dir cleanup, and reconnect timing markers.

Also fixed in the current tree: replacement-instance reconnect fallback now trusts a confirmed replacement `serverInstanceId` regardless of whether its snapshot revision is lower, equal, or newer than the request baseline, while force/downgrade remains limited to equal-revision and not-newer replacement snapshots. Backend reconnect coverage now includes a newer-revision replacement snapshot. Bad-live-event state recovery no longer clears on parsed-but-rejected state while the bad-event flag is pending, the flag declaration documents its producer/clear invariant, and local-session-missing errors now use one constructor instead of repeated typed 404 closures.

Also fixed in the current tree: post-commit SQLite integrity verification now has an explicit name, logs the full chmod/mode hardening chain before propagating a redirection failure, and the Unix state-directory redirection helper uses symmetric naming. Git-diff Markdown enrichment now enumerates non-git `ApiErrorKind` suppressions instead of hiding future variants behind a wildcard. `LocalSessionMissing` documents visible hydration, fallback, and remote-session-target producers. Backend reconnect tests now name fresh pre-deadline timers by their arm-time reference frame, use an explicit Scenario B elapsed-gap alias, drive bad-SSE recovery with canned response revisions, assert no fallback fetch fires before a live delta clears recovery, and cover failed manual retry preserving same-instance progress polling until live SSE proof.

Also fixed in the current tree: unhydrated session deltas whose target message is missing from the retained transcript no longer silently absorb into a metadata-only patch and wait on a possibly-wedged hydration. `applyDeltaToSessions` gained an `appliedNeedsResync` result variant that the SSE delta handler treats like `applied` for the metadata patch (sidebar preview / message count / status stay fresh) but additionally schedules `requestStateResync({ rearmOnFailure: true })` so the missing message body is fetched authoritatively. All five non-created delta types (`messageUpdated`, `textDelta`, `textReplace`, `commandUpdate`, `parallelAgentsUpdate`) return the new variant on the missing-target unhydrated branch, with table-driven coverage in `live-updates.test.ts` so a future refactor cannot drop the resync nudge from any one path. `SessionDeltaEvent` is now exported for test types.

Also fixed in the current tree: SSE Lagged-recovery snapshots are no longer silently ignored when their revision equals the client's current revision. The backend now emits a one-shot `lagged` event before the recovery snapshot in both Lagged branches of the SSE handler (state-receiver and delta-receiver), and the client listens for it and arms `forceAdoptNextStateEventRef = true` so the next state event is force-adopted regardless of revision parity. Without this marker, a slow consumer that fell past the broadcast channel capacity could receive the recovery snapshot and discard it as a redundant catch-up, leaving any messages carried by the dropped deltas hidden until another event with a strictly-greater revision arrived. New `App.live-state.reconnect.test.tsx` regression covers same-revision Lagged recovery without a reconnect open.

Also fixed in the current tree: the live-session resume watchdog now treats a user prompt at the turn boundary as in-turn activity for the purpose of stale-transport detection. The previous gate (`hasAssistantActivitySinceCurrentTurnBoundary`) returned false when the latest message was user-authored, so a session whose assistant first-chunk delta got dropped sat in "Waiting for the next chunk of output..." indefinitely with no automatic recovery — the user had to send another prompt or hard-refresh. The renamed `hasInTurnActivitySinceTurnBoundary` returns true for both assistant and user authors at the boundary, so once the staleness window elapses the watchdog fires and a `/api/state` resync surfaces whatever the lost delta would have streamed in. Two `App.live-state.*.test.tsx` regressions inverted from "does not watchdog-resync" to "watchdog-resyncs" pin the new behavior; a new `live-updates.test.ts` case covers the first-prompt-on-fresh-session shape directly.

Also fixed in the current tree: the SSE delta handler now force-triggers per-session re-hydration whenever `applyDeltaToSessions` returns `needsResync` or `appliedNeedsResync`, and likewise on the revision-gap branch. Previously the recovery path scheduled `requestStateResync` alone, but `/api/state` returns only the metadata-first summary — and `applyMetadataOnlySessionDelta` had already advanced the local mutation stamp to match what the delta carried. With matching stamps, `reconcileSummarySession` did not flip `messagesLoaded` back to false, so the hydration effect never re-fired and the missing message body stayed invisible until the user refreshed the page. `startSessionHydration(delta.sessionId)` now runs alongside the resync nudge: `hydratingSessionIdsRef` deduplicates so already-in-flight hydration is a no-op, and the targeted `/api/sessions/{id}` fetch is what actually surfaces the missing transcript. New `App.live-state.deltas.test.tsx` regression pins the hydrated-transcript missing-target path; the existing 1512 tests remain green.

Also fixed in the current tree (user-initiated restart path): the backend now performs a graceful persist-thread drain on Ctrl+C / SIGTERM so the very last `commit_persisted_delta_locked` reaches SQLite before the process exits. `axum::serve` is wired through `with_graceful_shutdown(shutdown_signal())` (a `tokio::signal` future that races Ctrl+C against SIGTERM on Unix), and after `serve` returns the new `AppState::shutdown_persist_blocking` sends `PersistRequest::Shutdown` and joins the worker thread. The persist worker drains every queued `Delta` (including any commit queued between the shutdown signal and the worker's next iteration), runs one final `collect_persist_delta` / `persist_delta_via_cache` pass, and only then exits. Closes the user-initiated-restart half of the durability window. Hard kills (SIGKILL, power loss) still race the same window — the worker has at most one un-drained mutation in flight, so the worst case is unchanged from before. New tests in `src/tests/persist.rs` cover the `PersistRequest::Shutdown` wait outcome (including during retry-backoff), the no-op-when-no-handle case, and a real-thread integration test that drains a queued Delta + Shutdown to completion. `tokio = ... features = [..., "signal", ...]` added in `Cargo.toml`.

Also fixed in the current tree: the graceful-shutdown wiring no longer hangs forever on Ctrl+C / SIGTERM when SSE clients are connected. The previous round's `with_graceful_shutdown(shutdown_signal())` blocked indefinitely because the SSE handler in `api_sse.rs::state_events` only exits its `tokio::select!` loop on `RecvError::Closed`, and the broadcast senders live on `AppState` clones — including the `shutdown_state` clone deliberately kept alive for the post-serve persist drain. Net effect of the regression: Ctrl+C blocked while a browser tab still had an open `/api/events`, the user had to force-kill, and the persist drain never ran (the same durability window the previous round was supposed to close). Fixed by adding `AppState::shutdown_notify: Arc<tokio::sync::Notify>` and threading it through both endpoints: the SSE handler pins a `Notified` future and calls `enable()` on it before its first poll (so a notification fired before the future is awaited still wakes it), and `main.rs::run_server` notifies all live waiters from inside the `with_graceful_shutdown` future immediately after `shutdown_signal()` resolves. SSE handlers see the notify, break their loops, and graceful shutdown then completes cleanly so the persist drain runs. New `tokio::test` regressions in `src/tests/persist.rs` pin both the standard "subscriber registered before notify" path and the race where the notify fires before subscription.

Also fixed in the current tree: the client now recovers automatically from a permanently-closed `EventSource`. The WHATWG spec mandates that an `EventSource` whose response has a non-200 status (or is otherwise rejected by the browser) transitions to `readyState === CLOSED` and stops auto-reconnecting — so even after a clean backend restart the live stream stayed dead, and the user had to hard-refresh the browser tab. This was particularly visible in the dev-mode setup because Vite's proxy (`ui/vite.config.ts:configureBackendUnavailableProxy`) returns `502 Bad Gateway` during the brief gap between the old backend exiting and the new one binding the port; that 502 reaches the browser as a non-200 SSE response and finalizes the close. Fixed by adding an `sseEpoch` state in `useAppLiveState`: when `onerror` fires with `eventSource.readyState === 2`, a recovery timer (exponential backoff, capped at 5s) bumps the epoch, the transport effect re-runs, the dead `EventSource` is closed, and a fresh one is constructed. `onopen` resets the recovery attempt counter and clears any pending recovery timer so the next failure cycle starts at the lowest backoff. The previously-missed `removeEventListener("lagged", …)` in the effect cleanup was added in the same change. A defensive 5-second SSE health watchdog also fires `scheduleSseEventSourceRecovery` if `readyState !== 1` (not OPEN) for 15 s, catching browsers/networks that leave the socket stuck in CONNECTING without firing `onerror` regularly. New `App.live-state.reconnect.test.tsx` regression simulates a `readyState === 2` error with all `/api/state` probes failing, asserts a fresh `EventSource` is constructed (so recovery does not depend on the `/api/state` fallback path).

Also updated in the current tree: `docs/architecture.md`'s SSE Event Stream section now documents all four event types (`state`, `delta`, `workspaceFilesChanged`, `lagged`) including the empty-`data` convention and forwarding policy for the new `lagged` marker, plus two new subsections covering the graceful-shutdown contract (`shutdown_signal` → `notify_waiters` → SSE break → graceful-shutdown completion → `shutdown_persist_blocking`) and the client-side EventSource recovery loop. `docs/metadata-first-state-plan.md` references to the renamed predicate `hasInTurnActivitySinceTurnBoundary` (was `hasAssistantActivitySinceCurrentTurnBoundary`) were updated with the broadened "user-prompt counts as in-turn activity" semantics.

Also fixed in the current tree: send-after-restart now updates the session preview tooltip and transcript within a single round trip instead of waiting for the 30 s `startStaleSendResponseRecoveryPoll` interval. When the user restarts the backend with the browser tab open and then sends a prompt, the POST response carries the new server's `serverInstanceId`, and `adoptState` (correctly) refuses to silently accept an unseen mismatched instance — so the local view stayed on the previous prompt's preview until the next safety-net poll fired half a minute later. `handleSend` now detects the rejection-by-instance-mismatch case via `isServerInstanceMismatch(lastSeen, response.serverInstanceId)` and fires `requestActionRecoveryResync({ allowUnknownServerInstance: true })` immediately, mirroring the pattern `adoptCreatedSessionResponse` already uses for cross-instance create responses. Stale same-instance rejections (e.g. SSE already advanced past this response's revision) still route through the existing 30 s poll because the tab is otherwise healthy. New `App.session-lifecycle.test.tsx` regression covers the restart-then-send case end-to-end.

Also fixed in the current tree (Bug #4 / `lagged` marker dispatch): both `RecvError::Lagged` branches in `src/api_sse.rs::state_events` now emit `Event::default().event("lagged").data("1")` instead of `.data("")`. The WHATWG EventSource spec lets browsers coalesce or skip frames without a `data:` line, so the empty-payload marker could silently fail to dispatch — leaving the same-revision recovery snapshot back in its original silently-ignored state. Clients are explicitly forbidden from parsing the byte; it's reserved as a control marker. The frontend regression at `App.live-state.reconnect.test.tsx` was updated to dispatch `data: "1"` to match the production wire shape, and `docs/architecture.md`'s SSE-events section now documents the `event: lagged\ndata:1\n\n` wire format and the rationale.

Also fixed in the current tree (Bug #1 / sticky SSE shutdown signal): the SSE shutdown path no longer relies on `tokio::sync::Notify::notify_waiters()`, which is one-shot and silently loses notifications fired before a waiter registers. Replaced by `tokio::sync::watch::channel(false)` exposed through new `subscribe_shutdown_signal` / `trigger_shutdown_signal` helpers on `AppState`. `state_events` calls `borrow_and_update()` BEFORE yielding the initial state and again before entering its `select!` loop; the new helper `wait_for_shutdown_signal` polls `changed()` with a `borrow_and_update()` re-check on each wake. `trigger_shutdown_signal` uses `send_replace(true)` instead of `send(true)` so the value is recorded even when there are zero live receivers — a `send` against an empty receiver set silently errors and would defeat the sticky contract. Two new `tokio::test` regressions in `src/tests/persist.rs` pin both subscriber-orderings (before-trigger AND after-trigger) end-to-end, both wrapped in `tokio::time::timeout(1s, …)` so a regression to a non-sticky implementation fails loudly instead of self-healing via an in-test re-notify.

Also fixed in the current tree (Bug #3 / graceful-shutdown durability regression): added a real integration test (`graceful_shutdown_drain_persists_final_mutation_across_reload` in `src/tests/persist.rs`) that drives the production-shaped path end-to-end — `AppState::new_with_paths` → real worker thread → `commit_locked` via `create_test_project` + `create_test_project_session` → `shutdown_persist_blocking` → reload via fresh `AppState::new_with_paths`. Without the graceful drain, the test fails because the just-committed session never reaches disk; with it, the reborn `AppState`'s `find_session_index` returns `Some(_)` for the durably persisted session id. The previous fake-loop test stays as a fast-path unit cover but is no longer the only thing pinning durability.

Also fixed in the current tree (Bug #2 / post-shutdown background mutations): `commit_delta_locked` now falls back to a synchronous full-state JSON write when the persist worker has exited. New `persist_worker_alive: Arc<AtomicBool>` field on `AppState` is flipped to `false` by `shutdown_persist_blocking` only AFTER the persist worker thread has joined (Acquire/Release ordering pairs the load with the flip), so any agent runtime / remote SSE bridge / orchestrator-resumer commit that lands after the worker is demonstrably gone goes straight to disk via the same fallback path the disconnected-channel test fixture already uses without racing the worker's final drain/write. `commit_locked`, `commit_persisted_delta_locked`, and `commit_session_created_locked` already had analogous fallbacks via `persist_internal_locked`'s `persist_tx.send` error branch — `commit_delta_locked` was the gap because it doesn't send its own persist signal (it relies on the worker draining future commits). New `commit_delta_locked_after_shutdown_falls_back_to_synchronous_persist` regression in `src/tests/persist.rs` proves a session preview mutated via `commit_delta_locked` AFTER `shutdown_persist_blocking` survives a reload.

Also fixed in the current tree (Rendered diff regions get prev/next navigation): the "Rendered" mode of the diff panel — `RenderedDiffView` (used for non-Markdown diffs that have renderable regions like Mermaid in `.mmd`, math, or markdown blocks inside `.ts`/`.rs`/etc.) — now exposes the same prev/next region navigation + `Region X of Y` counter that `MarkdownDiffView` and the Monaco file-diff view carry. The previous implementation rendered all regions as a single `composeRenderedDiffMarkdown` blob through one `MarkdownContent`, with `**Lines N–M**` headers inline in the synthetic markdown — that gave nothing for navigation to scroll to. The view is now refactored: each region renders in its own `<section data-rendered-diff-region-index="N">` wrapper with the `Lines N–M` header in JSX (so the wrapper element is the scroll target) and a per-region `MarkdownContent` for the body. New `composeRenderedDiffRegionMarkdown(region)` returns just the fenced body (` ```mermaid `, `$$…$$`, or raw markdown); the legacy `composeRenderedDiffMarkdown(regions)` whole-document assembler is retained for tests and external callers as a thin wrapper that prepends the header inline. The footer carries prev/next buttons (reusing `DiffNavArrow` + `.diff-preview-change-nav` / `.diff-preview-nav-button`) and shows "No rendered regions" when the region set is empty (buttons disabled). Wrap-around at the boundaries. Initial mount does NOT auto-scroll — the scroll-into-view side effect waits for the first user-driven navigation so the parent's restored scroll position is preserved. New `rendered-diff-view.test.tsx` covers nine cases: per-region wrapper with the data-attribute, prev/next wrap-around with `scrollIntoView({ block: "center" })`, empty region set with disabled buttons + "No rendered regions" copy, the Patch-only disclaimer, plus per-renderer body shape for mermaid/math/markdown and the legacy whole-document assembler.

Also fixed in the current tree (rendered Markdown diff change navigation): the rendered-Markdown diff view now has the same prev/next change navigation + `Change X of Y` counter that the Monaco file-diff view exposes. A new `ui/src/panels/markdown-diff-change-index.ts` module owns the pure walk that derives the change-block list from the segment model, mirroring the renderer's grouping rules (consecutive non-`normal` segments grouped, with the renderer's `added → removed` transition break preserved so navigation stops match the visible blocks 1:1). `MarkdownDiffView` now memoises the block list, tracks the current change index in React state, exposes prev/next buttons (reusing the existing `DiffNavArrow` icon and `.diff-preview-change-nav` / `.diff-preview-nav-button` styles from the Monaco footer), and an effect scrolls the active change-block into view via `[data-markdown-diff-change-index="N"]` (a new attribute emitted by `renderMarkdownDiffSegments`). The counter shows "No changes" when there are zero blocks and "Change N of M" otherwise; prev/next wrap at the boundaries. Initial mount intentionally does not auto-scroll — the scroll-into-view side effect waits for the first user-driven navigation so the parent's restored scroll position is preserved. New `markdown-diff-change-index.test.ts` covers the empty case, isolated changes between unchanged segments, removed→added grouping into one block, the renderer's `added → removed` break, and stable block-id identity across re-runs. New `DiffPanel.test.tsx` integration test "navigates between rendered Markdown change blocks via prev/next controls and a counter" drives a two-block markdown diff, asserts the counter advances 1→2→wrap-to-1 on next, 1→wrap-to-2→1 on prev, the matching `data-markdown-diff-change-index` values, and that `scrollIntoView({ block: "center" })` fires on each navigation.

Also fixed in the current tree (clipboard/Range helpers extracted from `markdown-diff-change-section.tsx`): the four pure DOM/Range helpers (`setDropCaretFromPoint`, `getSelectionRangeInsideSection`, `rangeCoversNodeContents`, `serializeSelectedMarkdown`) are now in a new sibling module `ui/src/panels/markdown-diff-clipboard-pointer.ts` (142 lines) with a header comment naming the cluster's contract and pointing back to the file it was split out of. The change-section file shrank from ~860 lines to ~795 lines and its header now explicitly defers the clipboard-pointer cluster to the new module alongside the other already-extracted neighbours (`markdown-diff-edit-pipeline`, `markdown-diff-caret-navigation`, `markdown-diff-segments`, `editable-markdown-focus`). React clipboard handlers (`handleCopy`, `handleCut`, `handleDrop`) stay in the change-section file and re-import the helpers via named exports. Pure code move per CLAUDE.md — no behaviour change, all 79 existing `DiffPanel.test.tsx` tests still pass.

Also fixed in the current tree (round-13 cross-instance non-send action recovery + send-after-restart live-stream proof + tripwire scoped to active transcript + post-shutdown persist write race): four follow-up fixes against the round-12 review's high-priority items. (a) `adoptActionState` in `app-session-actions.ts` now calls `forceSseReconnect()` when `isServerInstanceMismatch(lastSeen, state.serverInstanceId)` is true, mirroring the `handleSend` mismatch branch — approval / user-input / MCP elicitation / Codex app-request / settings actions returning from a restarted backend now recreate the EventSource alongside the `/api/state` recovery probe. Two new `app-session-actions.test.ts` regressions pin both halves: cross-instance recovery DOES call `forceSseReconnect`, same-instance stale rejection does NOT. (b) `App.session-lifecycle.test.tsx`'s "immediately probes /api/state when a send response carries an unseen serverInstanceId" test now dispatches `dispatchOpen()` + a replacement-instance state event + a `messageCreated` delta on the recreated `EventSourceMock` and asserts the assistant text rendered, proving the new transport actually delivers live deltas after recreation. (c) `App.live-state.restart-roundtrip.test.tsx` (the canonical tripwire) now scopes assertions to `document.querySelector(".message-card.bubble-assistant")?.textContent` instead of page-wide `getAllByText`, so a regression that only updates sidebar preview metadata can no longer satisfy the assistant-bubble check; step 4 dispatches a `textDelta` extending the partial chunk to the complete reply (the realistic SSE-resume after restart since the hydration response is rejected by `classifyFetchedSessionAdoption`'s `retainedMessagesMatch` check). (d) `shutdown_persist_blocking` in `src/sse_broadcast.rs` now flips `persist_worker_alive` to `false` AFTER `handle.join()` returns instead of BEFORE the Shutdown signal — the previous ordering raced concurrent `commit_delta_locked` synchronous fallback writes against the worker's still-in-progress final drain on the same persistence path. The new ordering ensures the synchronous fallback only fires when the worker has demonstrably exited. The `shutdown_persist_blocking` method-level doc was rewritten to match. A narrow post-collection-pre-join window remains for `commit_delta_locked` (worker captured final delta, hasn't joined yet, `alive` still true) — tracked in the active "Post-shutdown persistence writes still leave a post-collection-pre-join window" entry.

Also fixed in the current tree (canonical restart-roundtrip tripwire test): a new `App.live-state.restart-roundtrip.test.tsx` file owns one cross-layer integration test — "renders the streamed assistant reply through a full restart recovery chain without hard refresh" — that simulates the entire user-reported scenario in a single deterministic flow: pre-restart hydrated session with streaming partial assistant text → SSE re-open from replacement instance → `forceMessagesUnloaded` re-arms hydration → `/api/sessions/{id}` adopts the canonical transcript with the complete assistant reply → late `_sseFallback` reconnect probe at lower revision → `isEqualRevisionAutomaticReconnectSnapshot` guard rejects rollback. The test was confirmed to fail on the unfixed `forceMessagesUnloaded: false` (hydration never re-fires, pane stuck on streaming partial) AND on the unfixed `state.revision <= requestedRevision` clause (rollback hides assistant reply). The header comment names every fix point exercised by the scenario; a developer touching `app-live-state.ts`, `app-session-actions.ts`, `session-reconcile.ts`, `session-hydration-adoption.ts`, `live-updates.ts`, `state-revision.ts`, `api_sse.rs`, `sse_broadcast.rs`, `state.rs`, or `app_boot.rs` has a single tripwire to verify the cross-layer composition stays intact even when per-fix unit tests pass. The bug class produced 9+ distinct fixes across recent rounds; this single test catches the integration-shape regressions that no per-unit test could.

Also fixed in the current tree (same-instance reconnect lower-rev rollback): the SSE-error reconnect fallback `/api/state` probe (`scheduleReconnectStateResync`) no longer force-adopts a same-instance response whose `state.revision` is strictly lower than the request's `requestedRevision`. Same-server-instance revisions are monotonic — a lower response must be a stale snapshot from before the SSE deltas advanced local state, not a "newer" repair. The user-visible failure mode this closes: after a backend restart the user sends a new prompt, the assistant message streams in over SSE deltas, and the earlier-armed reconnect resync (whose 400ms timer captured `requestedRevision = 5` while local was at 5) finally returns at `state.revision = 4` because of request/response queuing — without the guard, force-adoption rolled local back to revision 4, the assistant message disappeared, no further deltas arrived, and the user had to hard-refresh. The fix renames `isNotNewerAutomaticReconnectSnapshot` → `isEqualRevisionAutomaticReconnectSnapshot` and tightens the comparison from `<=` to `===` (equal-revision adoption is still handled, replacement-instance rollback is still handled by `isNotNewerReplacementSnapshot`). New `App.live-state.reconnect.test.tsx` regression: "rejects a lower same-instance reconnect /api/state snapshot instead of rolling local state backward" — confirmed to fail on the unfixed `<=` and pass on the fixed `===`. Existing "restarts the resync loop from finally when a reconnect fallback queues behind a failing pre-reopen resync" test was updated from a deliberately-low revision-1 fixture to revision-2 (equal to the SSE state) so it pins the correct adoption path through `isEqualRevisionSnapshot` instead of the now-removed lower-revision branch.

Also fixed in the current tree (active-session staleness across restart): replacement-instance state adoption now forces `messagesLoaded: false` on summary-session reconciliation, so the visible-session hydration effect re-fires `GET /api/sessions/{id}` and the active pane is repainted with the server's authoritative transcript instead of staying stuck on the pre-restart streaming partial. Persisted sessions intentionally clear `sessionMutationStamp` on save/load (see `state-revision.ts::isStaleSameInstanceSnapshot`), so the post-restart summary arrives with no stamp signal; without an explicit "force re-hydration" hint, a coincidentally-matching `messageCount` would let `reconcileSummarySession` keep `messagesLoaded: true` against the local count and the hydration `useEffect` (which depends on `activeSession?.messagesLoaded`) would never re-fire. The fix threads a new `forceMessagesUnloaded` option through `AdoptSessionsOptions` → `reconcileSessions` → `reconcileSummarySession`, set by `resolveAdoptStateSessionOptions` whenever `serverInstanceChanged` is true. Three new regressions land alongside: (a) `session-reconcile.test.ts` proves the option flips `messagesLoaded` to `false` while retaining the previous `messages` array (so the pane has something to show until hydration completes); (b) `app-live-state.test.ts` proves `resolveAdoptStateSessionOptions` sets `forceMessagesUnloaded: true` exactly when `serverInstanceChanged` is true; (c) `App.live-state.reconnect.test.tsx` is an end-to-end App-level test that opens a session, drives an `_sseFallback` replacement-instance state event, and asserts `api.fetchSession("session-1")` is called — without the fix, the active pane stayed on the streaming partial until hard refresh.

Also fixed in the current tree (bookkeeping cleanup of the `lagged` SSE doc gap): the previously active "`lagged` SSE event not documented in architecture or plan docs" Medium entry is removed because the round-7 docs update already closed it. `docs/architecture.md`'s SSE Event Stream section enumerates all four event types (`state`, `delta`, `workspaceFilesChanged`, `lagged`) with the empty-`data` convention, the `event: lagged\ndata:1\n\n` wire format, the forwarding policy for the new marker, and dedicated subsections for the graceful-shutdown contract and the client-side EventSource recovery loop. `docs/metadata-first-state-plan.md` already references the renamed `hasInTurnActivitySinceTurnBoundary` predicate with the broadened "user-prompt counts as in-turn activity" semantics. The active ledger no longer carries a stale doc-coverage entry for either change.

Also fixed in the current tree (bookkeeping cleanup of the `lagged` listener leak entry): the previously active "`lagged` SSE event listener leaks if the cleanup path stops calling `eventSource.close()`" Medium entry is removed. `useAppLiveState`'s SSE transport effect cleanup now calls `removeEventListener("lagged", handleLaggedEvent as EventListener)` alongside the existing `state` / `delta` / `workspaceFilesChanged` listener removals, so a future regression that drops or reorders the implicit `eventSource.close()` teardown cannot leave the `lagged` handler attached and re-firing on the dead `EventSource`.

Also fixed in the current tree (bookkeeping cleanup of the `appliedNeedsResync` integration gap): the previously active "`appliedNeedsResync` end-to-end integration path has no targeted regression" Medium entry is closed by a new `App.live-state.deltas.test.tsx` case — "applies metadata patch immediately and hydrates when an unhydrated session receives a missing-target delta". The test deliberately leaves the unhydrated `messagesLoaded: false` session unopened (so the visible-session hydration effect cannot pre-fire and mask the handler-triggered path), dispatches a `textDelta` whose `messageId` is absent from the retained transcript, and then asserts both halves of the contract: (a) the sidebar `.session-preview` div's `title` attribute reflects the delta payload (proving the metadata patch landed synchronously via the `appliedNeedsResync` branch — not the OUTER session-row button's `agent / workdir` title), and (b) `/api/sessions/session-1` was fetched (proving `startSessionHydration(delta.sessionId)` ran alongside the resync nudge). The existing "force re-hydrates the session when a delta arrives whose target is missing on a hydrated transcript" test still pins the hydrated `needsResync` path, so both branches are now covered end-to-end.

Also fixed in the current tree (bookkeeping cleanup of the `persist_worker_alive` field doc): the stale `AppState` field-level comment in `src/state.rs` now matches the round-13 shutdown contract. It says `shutdown_persist_blocking` flips the flag only after the persist worker joins, so `commit_delta_locked`'s synchronous fallback cannot race the worker's final drain/write. The active High documentation entry for the old BEFORE-Shutdown wording was removed.

Also fixed in the current tree (streaming Markdown tables / fences / math no longer flicker through visibly-broken shapes): assistant replies that stream a pipe-table, fenced code block, or `$$ ... $$` math display block over multiple `textDelta` chunks no longer briefly render as raw `| ... |` text, runaway code, or table rows with mismatched cell counts before snapping to the canonical shape. New `ui/src/markdown-streaming-split.ts` owns a pure splitter `splitStreamingMarkdownForRendering(markdown)` that returns `{ settled, pending }`: the settled prefix is the part safe to pass through `react-markdown` (full GFM rendering), and the pending suffix is any in-flight trailing block that has not yet been terminated (pipe-table not closed by a blank line, fence/math without a matching closer). The splitter walks lines tracking fenced-code state (` ``` ` and `~~~`), `$$`-on-its-own-line math state, and pipe-line table state, then cuts at the earliest open-block start. Boundary newlines live at the end of `settled` so callers reconstruct the original via plain `settled + pending`. `MarkdownContent` (`ui/src/message-cards.tsx`) gains an `isStreaming` prop (default `false`); when `true` the settled half flows through the existing pipeline and the pending half renders as a styled `<pre class="markdown-streaming-fragment">` placeholder until the block closes and the next chunk fires the memoized re-split. The streaming assistant render path in the same file passes `isStreaming` automatically; settled callers (history bubbles, source-renderer previews, diff views) keep the existing pipeline unchanged. New `markdown-streaming-split.test.ts` covers the empty case, plain paragraphs, all four progressive partial-table states (header alone, header+separator, header+separator+partial row, block close), unclosed/closed fences (including pipes inside fence body), unclosed/closed math, the multi-block "earliest cut wins" rule, and a round-trip identity invariant. New `MarkdownContent.test.tsx` "isStreaming partial-table deferral" suite (7 cases) pins the same rendering contract end-to-end including the realistic mid-stream paragraph-then-partial-table shape and the explicit "settled callers without `isStreaming` are unchanged" guarantee. Mermaid / math budget counters and the line-marker scan now key on `settledMarkdown` so an in-flight unclosed block doesn't count against per-document caps. `cd ui && npx tsc --noEmit` clean; `cd ui && npx vitest run` green at 1,570 tests.

## Active Repo Bugs

## Markdown diff change-block grouping rules duplicated between renderer and index builder

**Severity:** Medium - the change-navigation index walker copies the renderer's grouping rules; future drift between the two will silently desynchronize navigation stops from rendered blocks.

`ui/src/panels/markdown-diff-view.tsx:508-526` and `ui/src/panels/markdown-diff-change-index.ts:60-87`. Both walks have identical logic: skip `normal`, gather consecutive non-`normal` segments, break at the same `current.kind === "added" && next.kind === "removed"` boundary, and produce identical id strings (`segments.map(s => s.id).join(":")`). The renderer then re-derives the same id and looks it up in a `Map<id, index>` the navigation code built from the index walker's output. The header comment in `markdown-diff-change-index.ts:46-54` explicitly acknowledges "the rule is duplicated here so the navigation index does not drift from what the user sees" — i.e., the only thing keeping the two walks in sync is the test suite.

**Current behavior:**
- Renderer (`renderMarkdownDiffSegments`) and index builder (`computeMarkdownDiffChangeBlocks`) walk the same segment array twice with identical grouping rules.
- The navigation index is recovered by id-lookup against a Map built from the index walker's output.
- Any future change to the grouping rules (e.g., a third break-rule for a new segment kind) must be made in both places.

**Proposal:**
- Have `renderMarkdownDiffSegments` consume the precomputed `changeBlocks` directly. Iterate (`normal` segment OR `changeBlocks[changeBlockCursor]`); the renderer emits the editable section for normals and the `<section>` wrapper for the next change-block, advancing `changeBlockCursor` after each.
- Single source of truth for grouping rules in `computeMarkdownDiffChangeBlocks`; the navigation index becomes the literal cursor position, no Map lookup needed, and the renderer's per-render Map allocation goes away.

## `state.rs` `persist_worker_alive` doc cross-link points to a removed `bugs.md` heading

**Severity:** Low - the doc-comment cross-references `bugs.md "Persist shutdown drain can run before background mutation sources are quiesced"`, but that heading was removed from the active ledger in this round.

`src/state.rs:273-274`. The "bookkeeping cleanup of the `persist_worker_alive` field doc" preamble entry in this round explicitly notes "The active High documentation entry for the old BEFORE-Shutdown wording was removed." The closest live entry is "Post-shutdown persistence writes still leave a post-collection-pre-join window", which is what the new doc-comment narrows-and-claims. A future reader who follows the link will hit a dead reference.

**Current behavior:**
- `state.rs` doc comment cross-links to a `bugs.md` heading that no longer exists.

**Proposal:**
- Re-target the cross-link to the live entry name: `bugs.md "Post-shutdown persistence writes still leave a post-collection-pre-join window"`.

## Markdown-diff and rendered-diff change/region counters lack `aria-live`

**Severity:** Low - screen-reader users get no announcement when the position counter advances or wraps; the only feedback is the button name, which is identical across clicks.

`ui/src/panels/markdown-diff-view.tsx:303-313` and `ui/src/panels/rendered-diff-view.tsx:266-275`. The "Change X of Y" / "No changes" / "Region X of Y" / "No rendered regions" status `<span>` is in a plain `<span className="source-editor-statusbar-state">` with no `aria-live` attribute. When the count changes (clicked Next, or count drops to 0 after a commit), assistive technology has no event to read out.

**Current behavior:**
- Counter span has no `aria-live` or `aria-atomic` attribute.
- Visually-impaired users navigating prev/next get no confirmation that the position changed past a wrap-around boundary, or that a commit just dropped the count to 0.

**Proposal:**
- Add `aria-live="polite"` (and optionally `aria-atomic="true"`) to the counter `<span>`. Polite is correct here — these updates are user-initiated, not urgent.

## `composeRenderedDiffMarkdown` legacy export retained but has no production callers

**Severity:** Low - the rendered-diff refactor kept the legacy whole-document assembler "for tests and external callers", but a grep finds no external callers; only its own test file imports it.

`ui/src/panels/rendered-diff-view.tsx:311-324`. The legacy assembler emits a different content shape from the new per-region renderer (`**Lines N–M**` headers inline in the markdown body) so its tests are not actually validating any production code path — they pin a shape only the legacy function emits.

**Current behavior:**
- `composeRenderedDiffMarkdown` is exported and tested but unused outside the test file.
- Future readers will spend cycles understanding why two compose helpers exist when only one is wired.

**Proposal:**
- Either (a) remove `composeRenderedDiffMarkdown` and its three test cases — `composeRenderedDiffRegionMarkdown` already has its own coverage and is what production calls; or (b) inline the helper into the test file if you want one external pinning of the per-region shape.

## `rangeCoversNodeContents` is exported but only used internally

**Severity:** Low - the clipboard-pointer extraction module exports a helper that is only used by another helper in the same file, widening the module's public API surface unnecessarily.

`ui/src/panels/markdown-diff-clipboard-pointer.ts:121`. `rangeCoversNodeContents` is consumed exclusively by `serializeSelectedMarkdown` in the same module. The new module's CLAUDE.md-style header comment lists it under owned exports, but no external caller exists.

**Current behavior:**
- Module-private helper exposed as a public export.
- Future refactors must consider phantom external consumers.

**Proposal:**
- Drop the `export` keyword on `rangeCoversNodeContents`; keep it module-private. Remove the corresponding bullet from the header's "owns" list (or downgrade it to "internal helper") so the public contract names only consumed exports.

## Concurrent shutdown callers can flip `persist_worker_alive` before the join owner finishes

**Severity:** Medium - the documented "flag flips only after worker join" contract is not true when two `AppState` clones call `shutdown_persist_blocking()` concurrently.

`shutdown_persist_blocking()` takes the worker handle out of `persist_thread_handle`, releases that mutex, and then blocks in `handle.join()`. A second concurrent caller can enter while the first caller is still joining, see `None`, and run the idempotent branch that stores `persist_worker_alive = false`. That lets a concurrent `commit_delta_locked()` take the synchronous fallback while the worker may still be doing its final drain/write, reopening the dual-writer persistence race the round-13 ordering was meant to close.

**Current behavior:**
- The first shutdown caller owns the join handle but does not hold the handle mutex while joining.
- A second shutdown caller treats `None` as "already stopped" even if the first caller is still waiting for the worker to stop.
- The second caller can publish `alive == false` before the worker has actually exited.

**Proposal:**
- Serialize the full shutdown transition so no caller can observe the stopped state until the join owner has returned from `handle.join()` and stored `persist_worker_alive = false`.
- Alternatively replace the `Option<JoinHandle>` state with an explicit `Running` / `Stopping` / `Stopped` state so only the join owner can transition from stopping to stopped.

## Rendered diff regions reset document-level Mermaid/math budgets

**Severity:** Medium - splitting rendered diff preview into one `MarkdownContent` per region weakens existing browser-side render-budget guards.

The rendered diff view now maps every renderable region to its own `MarkdownContent`. `MarkdownContent` counts Mermaid fences and math expressions per rendered document, so this split resets `MAX_MERMAID_DIAGRAMS_PER_DOCUMENT` and `MAX_MATH_EXPRESSIONS_PER_DOCUMENT` for each region instead of for the full diff preview. A crafted or simply large diff with many Mermaid/math regions can render far more expensive diagrams/equations than the previous single synthetic-document path allowed.

**Current behavior:**
- Each rendered diff region gets an independent Mermaid/math budget.
- The whole rendered diff preview no longer has one aggregate render cap.

**Proposal:**
- Compute aggregate Mermaid/math counts before mapping regions and apply a document-level fallback when the aggregate exceeds the cap.
- Or pass a shared render-budget context/override into each region-level `MarkdownContent`.

## Streaming table/math deferral is bypassed by the production assistant-message gate

**Severity:** Medium - the new streaming splitter can pass direct `MarkdownContent` tests while the real active assistant-message path still uses plain-text rendering for table-only or math-only streams.

`MessageCard` only passes `isStreaming` to `MarkdownContent` when `hasRenderableStreamingMarkdown()` returns true. That detector recognizes headings, lists, blockquotes, backtick fences, inline code, emphasis, and links, but it does not recognize pipe-table starts or standalone `$$` math blocks - two of the splitter's advertised deferral cases. Assistant output such as `Here is the table:\n\n| A | B |` or a standalone display-math stream therefore stays on `StreamingAssistantTextShell` and never reaches the pending-fragment placeholder until some other Markdown construct happens to trigger the gate.

**Current behavior:**
- `MarkdownContent` supports streaming split/placeholder rendering.
- `MessageCard` with active assistant text only reaches that path for constructs recognized by `hasRenderableStreamingMarkdown()`.
- Pipe-table and standalone `$$` math streams are missed by that gate.

**Proposal:**
- Centralize the streaming-structure detector with `markdown-streaming-split.ts`, or expand `hasRenderableStreamingMarkdown()` to include pipe tables, standalone `$$`, and tilde fences.
- Add production-path `MessageCard` regressions for active streaming pipe tables and math blocks.

## Streaming fence splitter does not enforce CommonMark closing-fence rules

**Severity:** Medium - unclosed fences can be treated as settled when their body contains a shorter or different fence marker.

`splitStreamingMarkdownForRendering()` currently toggles fence state on any line that starts with at least three backticks or tildes. CommonMark requires a closing fence to use the same character as the opener and to be at least as long as the opener. An open four-backtick fence containing a triple-backtick line, or a backtick fence containing a `~~~` line, can be incorrectly considered closed. The settled prefix can then be handed to `react-markdown` even though the real Markdown fence is still open.

**Current behavior:**
- The splitter tracks only `inFence` and the opening line index.
- It does not remember the opener character or opener length.
- Any backtick/tilde fence marker toggles the fence state while inside a fence.

**Proposal:**
- Store the opener marker character and length.
- Close only on the same marker character with length greater than or equal to the opener length.
- Add edge-case tests for four-backtick fences containing triple backticks and backtick fences containing tildes.

## Post-shutdown persistence writes still leave a post-collection-pre-join window

**Severity:** Medium - round-13 closed the dual-writer file race, but a narrow gap remains between the worker's final `collect_persist_delta` and `handle.join()` returning.

Round 13 moved the `persist_worker_alive` flip from BEFORE the Shutdown signal to AFTER `handle.join()` returns. That closed the dual-writer hazard (concurrent fallback writes racing the worker's still-in-progress final drain on the same persistence path). However, after the worker captures its final delta but before `handle.join()` returns, a concurrent `commit_delta_locked` will observe `alive == true`, bump `inner.mutation_stamp`, and return without persisting. That mutation is not picked up by the worker (already past collection) nor by the sync fallback (flag still true). `commit_locked` and `commit_persisted_delta_locked` are unaffected because they call `persist_internal_locked` which itself errors and falls back when the channel becomes disconnected.

**Current behavior:**
- The dual-writer file race is closed by round 13.
- A narrow window between the worker's final `collect_persist_delta` and `handle.join()` returning still exists; `commit_delta_locked` calls in that window observe `alive == true` and return without persisting.
- `commit_locked` / `commit_persisted_delta_locked` infer fallback from `persist_tx.send` failure, but `persist_tx` only disconnects when the LAST `AppState` clone drops its sender; with multiple clones (which is the production shape), `send` succeeds silently into a worker that has exited.

**Proposal:**
- Either (a) serialize the worker's final drain with sync fallback by holding `inner` for the worker's collect-and-write final iteration, or (b) require callers to quiesce non-HTTP producers before invoking `shutdown_persist_blocking`.
- Add a regression that races a late `commit_delta_locked` with the worker's final collection and proves the final persisted state is the latest `StateInner`.
- Add an explicit `persist_worker_alive` Acquire check to `persist_internal_locked` so all four commit variants share one shutdown contract.

## `architecture.md` "Graceful shutdown contract" still describes the pre-round-12 `Notify` mechanism

**Severity:** Medium - documented protocol contract no longer matches the implementation; future remote-bridge or replacement-handler implementers will read the wrong invariant.

The "Graceful shutdown contract" subsection in `docs/architecture.md` says `with_graceful_shutdown` first calls `state.shutdown_notify.notify_waiters()` and that every live `/api/events` stream's "pinned `Notified` future resolves and breaks the handler's `tokio::select!` loop". Round 12 replaced `Notify` with `tokio::sync::watch::channel(false)`, exposed `subscribe_shutdown_signal` / `trigger_shutdown_signal` helpers, and `state_events` now uses `borrow_and_update()` plus `wait_for_shutdown_signal` polling `changed()` with re-checks. The same architecture.md edit pass updated the `lagged` event description but missed the shutdown contract paragraph.

**Current behavior:**
- The architecture doc describes a `Notify`-based pinned-future mechanism that no longer exists.
- The actual implementation uses `tokio::sync::watch::Sender<bool>` with `send_replace(true)` for sticky-broadcast semantics.
- The "sticky" property (a subscriber registered AFTER the trigger still observes `true`) is the load-bearing protocol invariant that `Notify` did NOT have; the doc still describes the broken non-sticky mechanism.

**Proposal:**
- Replace the contract paragraph with the watch-channel description: `trigger_shutdown_signal()` calls `send_replace(true)`; `state_events` performs dual `borrow_and_update()` checks (pre-yield and inside the select loop); subscribers registered between Ctrl+C and graceful-shutdown completion still observe the sticky `true` value.

## Send-after-restart live-stream proof assertion is unscoped to the active transcript

**Severity:** Medium - the round-13 extension dispatching a post-restart delta on the new EventSource asserts via page-wide `getAllByText`, which the same delta's `preview` field can satisfy through the sidebar tooltip alone.

`App.session-lifecycle.test.tsx`'s "immediately probes /api/state when a send response carries an unseen serverInstanceId" test was extended to dispatch `dispatchOpen()` + a replacement-instance state event + a `messageCreated` delta on the NEW `EventSourceMock`. The new assertion `expect(screen.getAllByText("Recovered through the new EventSource.").length).toBeGreaterThan(0)` matches the same string in the sidebar session preview's `title` attribute (because the dispatched delta's `preview` field carries the same text). A regression where deltas update preview metadata but not the active transcript bubble would still satisfy this assertion — exactly the failure mode the new tripwire test was created to close.

**Current behavior:**
- The test asserts the recovered text appears anywhere on the page.
- The dispatched delta's `preview` field carries the same text, populating the sidebar tooltip independently of the active transcript.

**Proposal:**
- Scope the assertion to the active assistant bubble: `expect(document.querySelector(".message-card.bubble-assistant")?.textContent).toContain("Recovered through the new EventSource.")`. Mirrors the scoping pattern in `App.live-state.restart-roundtrip.test.tsx`.

## `markLiveSessionResumeWatchdogBaseline` lacks invariant cross-link to the watchdog `<=` rationale

**Severity:** Medium - the load-bearing guarantee for the reconnect (`===`) vs watchdog (`<=`) asymmetry lives entirely in a comment in `app-live-state.ts`; a refactor narrowing the baseline-update set could silently invalidate the asymmetry's safety reasoning.

The watchdog `<=` retention rests on a non-obvious cross-module invariant: every session-content delta MUST update `markLiveSessionResumeWatchdogBaseline`, so that the watchdog cannot fire while session-content deltas are recent. The inline comment at `app-live-state.ts:1844-1869` documents this excellently for that one site. There is no source-level cross-link from `markLiveSessionResumeWatchdogBaseline`'s definition back to this asymmetry — a future refactor that narrows the baseline-update set (e.g., excluding orchestrator metadata changes) would silently invalidate the reasoning.

**Current behavior:**
- The asymmetry rationale is documented inline in the consumer.
- The producer (`markLiveSessionResumeWatchdogBaseline`) has no doc explaining what depends on its calling discipline.

**Proposal:**
- Add a short doc comment on `markLiveSessionResumeWatchdogBaseline` naming the invariant: "any session-content delta MUST update this baseline; the watchdog branch in `adoptState`'s reconnect/fallback handler relies on this to keep the `<=` rollback semantics safe (see `isEqualRevisionAutomaticReconnectSnapshot` for the asymmetry rationale)."
- Optional: add a short subsection to `docs/architecture.md` describing both gates and the rationale for the asymmetry (already requested in the watchdog Note entry below).

## `isEqualRevisionAutomaticReconnectSnapshot` predicate is logically dead after the L197 tightening

**Severity:** Low - the predicate's only differentiating contribution disappeared when `<=` was tightened to `===`; the branch is now subsumed by `isEqualRevisionSnapshot`.

After the round-12 L197 tightening from `state.revision <= requestedRevision` to `state.revision === requestedRevision`, both `isEqualRevisionAutomaticReconnectSnapshot` and `isEqualRevisionSnapshot` require equal-same-instance-revision force-adoption. The reconnect-specific predicate's only remaining differentiator was the `(preserveReconnectFallback || rearmOnSuccess && rearmUntilLiveEventOnSuccess && !rearmAfterSameInstanceProgressUntilLiveEvent)` flag gate. But `isEqualRevisionSnapshot` already permits force-adoption regardless of those flags via the OR in `shouldForceAuthoritativeSnapshot` — so the entire reconnect-specific branch is unreachable as a unique trigger. Functionally correct but tempts future re-tightening of the wrong predicate.

**Current behavior:**
- `shouldForceAuthoritativeSnapshot` ORs `isEqualRevisionSnapshot || isNotNewerReplacementSnapshot || isEqualRevisionAutomaticReconnectSnapshot || shouldTrustWatchdogSnapshot`.
- Whenever `isEqualRevisionAutomaticReconnectSnapshot` would fire, `isEqualRevisionSnapshot` already fired one term earlier with the same predicate.

**Proposal:**
- Either drop `isEqualRevisionAutomaticReconnectSnapshot` (lean on `isEqualRevisionSnapshot`), or add an inline comment that it is intentionally retained as a named alias for symmetry with `shouldTrustWatchdogSnapshot` / `isNotNewerReplacementSnapshot` even though it is subsumed.

## "Rejects a lower same-instance reconnect" test does not pin polling continuation

**Severity:** Low - the new L197 regression asserts the rollback is rejected but stops there; a regression that incorrectly clears the polling loop after rejection would still pass.

`App.live-state.reconnect.test.tsx`'s new "rejects a lower same-instance reconnect /api/state snapshot" test verifies one `fetchState` call and that the rolled-back text isn't visible. It does not advance timers further to confirm reconnect-fallback polling stays armed until SSE reopens. The companion test "keeps reconnect fallback polling armed after replacement-instance fallback adoption until SSE reopens" covers polling continuation for the replacement-instance case but not for the rejected-stale-same-instance case.

**Current behavior:**
- The test asserts one `fetchState` call and absence of rolled-back text.
- No timer advancement after the rejection, so polling-continuation is unverified.

**Proposal:**
- After asserting the rejection, call `await advanceTimers(RECONNECT_STATE_RESYNC_DELAY_MS)` and assert `fetchStateSpy` fires a second time (proves the loop stayed armed despite the rejection).

## Watchdog/reconnect asymmetry not documented in `docs/architecture.md`

**Severity:** Low - the bugs.md "Watchdog recovery still allows lower same-instance revisions" entry's proposal said to document the asymmetry in architecture.md; the round-12 doc edit only updated the `lagged` event format.

The reconnect path requires `state.revision === requestedRevision` for same-instance force-adoption (closed L197). The watchdog path keeps `state.revision <= requestedRevision` for orchestrator-revision-noise recovery (because the watchdog cannot fire while session-content deltas are recent). Both inline comments document the rationale, but a reviewer reading `docs/architecture.md` alone has no signal that the asymmetry is a deliberate choice — they are likely to re-flag it.

**Current behavior:**
- Inline comments in `app-live-state.ts` document the asymmetry.
- `docs/architecture.md` has no description of either gate.

**Proposal:**
- Add a short subsection to `architecture.md` (e.g., under the SSE Event Stream section or a new "Live-state reconnection and watchdog recovery" topic) describing both gates and the rationale for the asymmetry: reconnect probes can race new SSE assistant deltas (so `===` is required to prevent rollback past streamed content), while watchdog probes only fire after stale-transport detection (so `<=` is safe AND load-bearing for orchestrator-revision-noise recovery).

## Watchdog recovery still allows lower same-instance revisions for orchestrator-noise recovery

**Severity:** Note - the watchdog path keeps `state.revision <= requestedRevision` deliberately, after architecture-review pushback that proposed mirroring the L197 reconnect tightening was rejected for breaking legitimate recovery cases.

The reconnect fallback path was tightened to require `state.revision === requestedRevision` for same-instance force-adoption (closes a real rollback hazard where a late `/api/state` response queued behind newer SSE assistant deltas could roll local state back past the streamed reply). A round-12 review proposed applying the same `===` clamp to the watchdog branch. Two existing tests proved this would regress legitimate recovery: `App.live-state.deltas.test.tsx` "watchdog-resyncs when only orchestrator deltas arrive during stale live transport", and `App.session-lifecycle.test.tsx` "resyncs on the first wake-gap tick for a newly created active session before any SSE arrives". The L197 hazard does NOT apply through the watchdog path because session-scoped deltas update `lastLiveSessionResumeWatchdogTickAtBySessionId` baselines via `markLiveSessionResumeWatchdogBaseline`, so the watchdog cannot fire while session-content deltas are still recent. The watchdog only fires after stale-transport detection (no recent activity for `LIVE_SESSION_RESUME_WATCHDOG_DRIFT_MS`), at which point a lower-revision /api/state response is the canonical recovery mechanism, not a stale rollback.

**Current behavior:**
- Reconnect snapshots require equal same-instance revision before force-adoption (`isEqualRevisionAutomaticReconnectSnapshot`).
- Watchdog snapshots retain `state.revision <= requestedRevision` for orchestrator-revision-noise recovery.
- The watchdog branch carries an inline comment explaining the asymmetry.

**Proposal:**
- Keep the watchdog branch at `<=`; the `===` clamp would break the orchestrator-noise recovery path that drives the two existing regressions named above.
- Optional follow-up: refactor the watchdog to trigger per-session hydration (`/api/sessions/{id}`) instead of `/api/state`, so the global revision counter is never rolled back. That removes the asymmetry without breaking recovery; it is a larger refactor than the L197 fix justified.
- Document the asymmetry as accepted in `docs/architecture.md` so future reviewers don't re-flag it.

## Lagged SSE wire format lacks backend route-level coverage

**Severity:** Medium - the browser-facing `event: lagged` serialization can regress without a backend test catching it.

The prior production bug was that an empty-data SSE control frame may not dispatch in browsers. Frontend tests that manually dispatch `lagged` with `data: "1"` do not prove the backend route actually serializes `event: lagged` with a non-empty `data:` line before the recovery `state`.

**Current behavior:**
- Frontend tests synthesize the `lagged` event.
- Backend tests do not assert the raw `/api/events` wire frame contains `event: lagged` and `data: 1`.

**Proposal:**
- Add route-level `/api/events` coverage that forces receiver lag.
- Assert the raw SSE frame emits `event: lagged`, a non-empty `data: 1` line, and then the recovery `state`.

## SSE recreation control plane is split between `sseEpoch` state and `pendingSseRecreateOnInstanceChangeRef`

**Severity:** Medium - two coordination mechanisms for one concern, increases regression risk and reduces debuggability.

`forceSseReconnect()` sets `pendingSseRecreateOnInstanceChangeRef.current = true` synchronously and the consume happens inside `adoptState` only when `fullStateServerInstanceChanged` is true. This adds a second control plane for SSE reconnection alongside the existing `sseEpoch` state, with the ref-vs-state ordering being load-bearing (synchronous `setSseEpoch` would tear down the in-flight probe). The pattern is documented inline, but ref state is not visible in React DevTools or in any state diff, so a subsequent maintainer reading the SSE transport effect cannot see the gate that determines whether the effect re-runs. The Round 8 comment in the doc-block notes this exact split-plane pattern was reverted before for the same reason. There is also no clear-on-no-instance-change reset path: if `forceSseReconnect()` fires but the recovery probe response comes back as same-instance (false alarm), the flag stays armed and could fire on a much later legitimate restart.

**Current behavior:**
- `forceSseReconnect()` mutates a ref invisible to DevTools.
- The flag is consumed only inside the `fullStateServerInstanceChanged` branch of `adoptState`.
- A successful recovery probe with no instance change leaves the flag armed indefinitely.
- The flag-on-adopt ordering relative to `setSseEpoch` is not pinned by a load-bearing test (current tests assert the recreate happens but not the ordering).

**Proposal:**
- Lift the gate into a state-driven shape (e.g., a single `sseReconnectReason` state with `instanceChangeAfterAdopt` as one of its values), so the SSE reconnection trigger is visible in React DevTools.
- Or add a load-bearing test that fails if the consume-on-adopt ordering is reversed.
- Either way, clear `pendingSseRecreateOnInstanceChangeRef` on any `adoptState` success that does not change the instance, so a false-alarm `forceSseReconnect()` cannot fire on a later legitimate restart.

## Sticky shutdown tests bypass `/api/events` stream wiring

**Severity:** Medium - helper-level tests can pass while the production SSE handler still hangs during shutdown.

The new tests validate the sticky `watch` shutdown helper directly, but they do not exercise `state_events` or the `/api/events` route using that signal. A future regression in the stream's pre-loop checks or select wiring could keep long-lived SSE connections open and block graceful shutdown while the helper tests still pass.

**Current behavior:**
- Tests cover the shutdown signal helper before/after registration.
- They do not hold or open `/api/events` streams and assert termination through the route handler.

**Proposal:**
- Add route-level SSE shutdown tests for shutdown-before-connect and shutdown-after-initial-state.
- Wrap both in timeouts so missed shutdown delivery fails loudly.

## Remote SSE bridge ignores `lagged` recovery markers

**Severity:** Medium - `src/remote_sync.rs:889-930`. `/api/events` is also consumed by the remote SSE bridge, but `dispatch_remote_event` currently handles only `state` and `delta`. Once the browser-visible `lagged` marker is fixed, remote streams still will not use it to force the following same-revision recovery snapshot.

That leaves remote clients exposed to the same Lagged recovery hole the browser path is trying to close: the remote bridge can ignore the marker, process the recovery `state` through existing revision gates, and discard a snapshot whose revision matches what it already recorded.

**Current behavior:**
- `process_remote_event_stream` parses arbitrary SSE event names.
- `dispatch_remote_event` ignores unknown names, including `lagged`.
- Remote fallback handling exists for `state` payloads with `_sseFallback`, but there is no one-shot marker for native Lagged recovery snapshots.

**Proposal:**
- Teach the remote event processor to treat `lagged` as a marker for the next `state` event from the same stream.
- Either force-apply that next remote state snapshot when it is same-revision, or trigger a force-capable `resync_remote_state_snapshot`.
- Add a remote SSE test that sends `lagged` followed by same-revision `state` and proves the recovery path is honored.

## Shutdown signal registration errors can look like real shutdown

**Severity:** Medium - `src/main.rs:147-166`. The new `shutdown_signal()` helper ignores `tokio::signal::ctrl_c().await` errors, and on Unix the SIGTERM branch completes immediately if `tokio::signal::unix::signal(...)` returns `Err`.

Those error paths should be diagnostics or startup failures, not successful shutdown triggers. If signal registration fails, the server can exit immediately after startup with little context.

**Current behavior:**
- Ctrl+C signal errors are discarded with `let _ = ...`.
- Unix SIGTERM registration failure makes the `terminate` future complete.
- The `tokio::select!` cannot distinguish a real shutdown signal from a signal-listener setup failure.

**Proposal:**
- Make signal setup fallible during startup and return an error if registration fails.
- Or log the registration/await error and park that branch with `std::future::pending::<()>().await` so it cannot trigger shutdown.

## Final shutdown persist failure exits without retry

**Severity:** Medium - `src/app_boot.rs:270-275`. The normal persist worker records failures and retries with backoff, but a shutdown tick sets `should_exit_after_tick` and breaks after the first final attempt even if that attempt failed.

A transient SQLite lock, disk hiccup, or I/O error during graceful shutdown can still drop pending mutations. The new drain logs the failure, but the process continues toward exit as though the final state reached disk.

**Current behavior:**
- `retry_state.record_result(&result)` records the final failure.
- `should_exit_after_tick` still breaks the loop immediately.
- Pending changed sessions can remain only in memory when the process exits.

**Proposal:**
- On shutdown, exit only after a successful final persist.
- Or use a bounded retry/timeout policy and return/log a shutdown failure outcome that clearly says durability was not confirmed.
- Add a test covering `Err` followed by `Ok` after `PersistRequest::Shutdown`.

## Triplicate `requestStateResync + startSessionHydration` recovery pattern in delta handler

**Severity:** Low - `ui/src/app-live-state.ts:2329, 2421, 2437`. Three near-identical recovery sites within ~110 lines of the same handler perform the same `requestStateResync({ rearmOnFailure: true }) + startSessionHydration(delta.sessionId)` pair. The `appliedNeedsResync` branch knows `delta.sessionId` is statically a string; the other two branches add a runtime guard (`"sessionId" in delta && typeof delta.sessionId === "string"`) — the type narrowing is subtly different at each site.

A future fourth recovery branch would need to update three sites; collapsing into a helper subsumes the gate and centralizes the contract comment.

**Proposal:**
- Extract `function triggerRecoveryForDelta(delta: DeltaEvent)` that performs the resync and conditional hydration.
- Replace the three call sites with the helper. Centralize the contract comment.

## Two backend Lagged branches duplicate the lagged-marker emission

**Severity:** Low - `src/api_sse.rs:182-200, 204-215`. The state-receiver and delta-receiver Lagged branches now both yield `lagged` followed by a recovery state snapshot built via `state_snapshot_payload_for_sse(state.clone()).await`. The branches are byte-identical apart from comments. The third Lagged branch (`file_receiver` at line 221) deliberately doesn't recover — so a 2-of-3 helper is still warranted for the asymmetric maintenance risk: a future change that grows one branch (e.g., a tracing log, structured `data` body, or `revision` hint on the marker) needs to be mirrored manually on the other.

**Proposal:**
- Extract a helper that yields the marker + recovery snapshot. The `async_stream::stream!` macro doesn't compose cleanly with helpers that themselves yield, so consider a named local closure or document the invariant explicitly.
- Or, accept the duplication and add cross-referencing comments naming both branches.

## `lagged` force-adopt arming is not scoped to the recovery baseline

**Severity:** Medium - `ui/src/app-live-state.ts:2479-2492` (`handleLaggedEvent`) and the state adoption call around `ui/src/app-live-state.ts:2158-2163`. The contract from the backend is "the next state event on this stream is the recovery snapshot to force-adopt", but the frontend stores only an unscoped boolean. If local state advances after the marker but before the recovery snapshot arrives, or if the stream reconnects before the marker is consumed, the next state event can be force-adopted with revision downgrade enabled even though it is no longer the intended same-baseline repair.

Lagged recovery only needs to bypass same-revision rejection for the recovery snapshot current at the time lag was detected. It should not give an unrelated later snapshot permission to roll the client backward.

**Current behavior:**
- `handleLaggedEvent` sets `forceAdoptNextStateEventRef.current = true` unconditionally.
- The flag is consumed by the very next `state` event the client receives, regardless of whether local revision advanced in between.
- The forced state adoption path also enables `allowRevisionDowngrade`.
- The flag can survive a reconnect boundary until the next `state` event consumes it.

**Proposal:**
- Store the local revision when `lagged` is received and only force the next state if the client is still at that baseline.
- Prefer same-revision bypass for Lagged recovery instead of allowing lower same-instance revision adoption.
- Clear any unconsumed Lagged force-adopt marker on `eventSource.onerror`, or otherwise scope it to the current stream.

## Per-session hydration burst has no cooldown beyond in-flight deduplication

**Severity:** Low - `ui/src/app-live-state.ts:2329, 2421, 2437`. The new `startSessionHydration(delta.sessionId)` calls trigger `GET /api/sessions/{id}` (full transcript fetch) on every problematic delta. `hydratingSessionIdsRef` deduplicates concurrent fetches per session, but it does not rate-limit successive fetches: once a hydration completes, the next problematic delta on the same session immediately schedules another full transcript fetch. On a flaky network with bursty deltas, a hydration→delta→hydration loop is possible, each iteration shipping the entire transcript over the wire.

**Current behavior:**
- In-flight dedup via `hydratingSessionIdsRef` collapses simultaneous calls to one round-trip.
- After completion, the next problematic delta immediately schedules another fetch with no cooldown.
- Phase-1 local-only deployment makes this practically free; future remote-host or flaky-network use exposes the storm risk.

**Proposal:**
- Add a per-session cooldown timestamp ("don't re-hydrate the same session within Nms of the last completed hydration unless the new delta carries a revision strictly greater than the one that started the previous hydration").
- Or document the burst as intentional given the local-only deployment cost; add a comment naming the trade-off so future reviewers don't keep flagging it.

## No-worker persist shutdown idempotence test has no assertion

**Severity:** Low - `src/tests/persist.rs:956`. The `shutdown_persist_blocking_is_idempotent_when_no_worker_handle` regression currently calls the method twice but asserts nothing. It proves only that the method does not panic in the test shape.

If a future refactor accidentally sends `PersistRequest::Shutdown` on a no-worker test channel or mutates the handle unexpectedly, this test would still pass.

**Current behavior:**
- The test exercises the no-worker branch twice.
- It does not assert the handle remains `None`.
- It does not assert no shutdown signal was sent to the test receiver.

**Proposal:**
- Assert `persist_thread_handle` remains `None` after both calls.
- If the test owns a receiver, assert no `PersistRequest::Shutdown` was produced.

## Watchdog-inversion tests don't assert the "Waiting for the next chunk of output…" affordance state

**Severity:** Low - `ui/src/App.live-state.deltas.test.tsx:3439` and `ui/src/App.live-state.watchdog.test.tsx:625`. The two recent inverted tests assert that the recovered text becomes visible, but say nothing about the "Waiting for the next chunk of output…" affordance. After the recovery snapshot adopts (the deltas test's snapshot has `status: "idle"`, the watchdog test's stays `status: "active"`), the affordance state is the most user-visible signal of whether recovery actually replaced the wedged UI vs just rendered the recovered text somewhere on the page.

**Proposal:**
- In `deltas.test.tsx`: add `expect(screen.queryByText("Waiting for the next chunk of output...")).not.toBeInTheDocument();` after the assertion that the recovered chunk is visible (recovery snapshot is idle, affordance should disappear).
- In `watchdog.test.tsx`: add an assertion clarifying expected affordance state for the still-active recovery (the assistant chunk now sits at the boundary, so the affordance should NOT be present).

## Stale-ignored-delta negative case not pinned for `pendingBadLiveEventRecovery` branching

**Severity:** Low - `ui/src/backend-connection.test.tsx`. The new "applied delta clears recovery" test pins the apply-branch reset of `pendingBadLiveEventRecovery`, but the parallel stale-ignored-delta negative case (where a delta with `revision < latestStateRevisionRef.current` after a bad-state payload does NOT clear recovery) is implicit — covered only by the absence of a fetch count change in the existing tests.

**Proposal:**
- Add a regression where the dispatched delta has `revision < latestStateRevisionRef.current` (stale ignored delta) after a bad-state payload, asserting polling continues.

## Rendered Markdown diff navigation does not scroll when there is exactly one change

**Severity:** Low - prev/next buttons can appear to do nothing for the common "one changed block" case.

`MarkdownDiffView` and `RenderedDiffView` intentionally skip the initial scroll so restored parent scroll position is preserved. Navigation scrolls from a `useEffect` keyed on the current index. When there is exactly one change/region, pressing next or previous computes the same index, React bails out of the state update, and the scroll effect does not run. The controls remain enabled but cannot bring the lone target into view.

**Current behavior:**
- Initial mount skips scrolling by design.
- One-change/one-region navigation resolves to the same index.
- No separate "navigation requested" signal exists to force a scroll to the same target.

**Proposal:**
- Drive the scroll side effect from a navigation request counter, or call an explicit scroll helper from the prev/next handlers.
- Cover both `MarkdownDiffView` and `RenderedDiffView` one-target cases.

## Rendered diff region navigation has no explicit scroll-container layout contract

**Severity:** Low - the new region-navigation ref may target a wrapper that is not the actual scroll container.

`RenderedDiffView` introduces `.diff-rendered-view-scroll` and queries it for `data-rendered-diff-region-index` targets, but the changed CSS does not give that wrapper an explicit flex/overflow contract, and the component does not adopt the existing `source-editor-shell source-editor-shell-with-statusbar` layout used by the Monaco and Markdown diff modes. If the parent remains the real scroller, `scrollIntoView()` may work inconsistently and the statusbar can diverge from the rest of the diff editor surface.

**Current behavior:**
- `RenderedDiffView` owns a new internal scroll ref.
- `.diff-rendered-view-scroll` has no explicit overflow/flex sizing.
- The rendered diff footer is not wrapped in the established editor shell/statusbar structure.

**Proposal:**
- Either adopt the existing editor-shell/statusbar layout contract or add explicit CSS that makes `.diff-rendered-view-scroll` the intended scroll container.
- Add a focused layout/navigation regression for rendered-region scrolling.

## Post-commit hardening helpers have no automated production-path coverage

**Severity:** Low - `src/persist.rs:213-227`. `verify_persist_commit_integrity` is `#[cfg(not(test))]`-only because it depends on production SQLite path hardening. The post-commit contract - redirection remains fatal, owner-only chmod/mode verification remains fatal unless `TERMAL_ALLOW_INSECURE_STATE_PERMISSIONS` is set - has no direct automated coverage.

**Proposal:**
- Expose a testable seam (e.g., inject the hardening function via a closure or trait), OR
- Add a Linux-only integration test that creates a real chmod-failing scenario.

## Watchdog wake-gap stops-after-progress invariant is not pinned

**Severity:** Low - `ui/src/backend-connection.test.tsx`. No direct negative-case test pins that watchdog wake-gap reconnect probes (which do NOT set `pendingBadLiveEventRecovery`) STOP after same-instance snapshot progress without a data-bearing SSE event. The cause-specific flag's whole premise is that wake-gap probes can stop while parse/reducer-error probes keep polling, but only the polling-continues side is pinned.

**Proposal:**
- Add a regression that triggers a watchdog wake-gap reconnect (no parse/reducer error), receives a same-instance progressed `/api/state` snapshot, advances `RECONNECT_STATE_RESYNC_MAX_DELAY_MS`, and asserts `countStateFetches()` did not increment.





## Workspace file hints have no bad-live-event recovery coverage

**Severity:** Low - `ui/src/app-live-state.ts:2366`. `workspaceFilesChanged` events intentionally do not confirm recovery while `pendingBadLiveEventRecovery` is active, but no direct test pins that non-authoritative file-change hints keep reconnect polling alive.

A workspace file-change hint proves the SSE connection can deliver some event type, but it does not prove state or delta delivery recovered after a malformed reopened-SSE payload. Without coverage, a future cleanup could accidentally restore the old confirmation behavior and clear reconnecting before state/delta events are healthy again.

**Current behavior:**
- Bad reopened state/delta payloads set `pendingBadLiveEventRecovery`.
- `workspaceFilesChanged` skips `confirmReconnectRecoveryFromLiveEvent` while that flag is active.
- Tests cover data-bearing state/delta confirmation paths, but not this non-authoritative event type.

**Proposal:**
- Add a regression that dispatches a bad reopened SSE payload, then a `workspaceFilesChanged` event, and asserts reconnecting state plus fallback polling remain active until a state/delta event confirms recovery.

## `app-live-state.ts` reconnect state machine continues to grow

**Severity:** Low - `ui/src/app-live-state.ts:2504 lines`. TS utility threshold (1500) exceeded; new `pendingBadLiveEventRecovery` adds another flag-shaped piece of reconnect bookkeeping. The reconnect/resync state machine inside `useEffect` now coordinates 6+ pieces of cross-cutting state.

**Proposal:**
- Extract a `ReconnectStateMachine` (or similar) module that owns the flag set + transitions and exposes named events (`onSseError`, `onSseReopen`, `onBadLiveEvent`, `onSnapshotAdopted`, `onLiveEventConfirmed`).
- Defer to a pure code-move commit per CLAUDE.md.


## `select_visible_session_hydration_fallback_error` lacks integration coverage

**Severity:** Low - `src/state_accessors.rs:351-369`. Unit tests pin the helper and typed local-miss fallback in isolation, but no integration-style test asserts the public `get_session` path returns 404 to the caller when a recoverable remote error is followed by a `not_found` fallback. A future refactor that drops the selector call from the `or_else` chain would not be caught.

**Current behavior:**
- Selector is unit-tested but the wiring that makes the new behavior reach a caller is not pinned.

**Proposal:**
- Add an integration-style test that drives the public `AppState::get_session` path through a recoverable remote hydration miss followed by a vanished cached summary, and asserts the response is `404 session not found` / `LocalSessionMissing` rather than the original recoverable remote error.

## `useAppSessionActions` ref cluster has grown from 1 to 4 to feed the rejected-action classifier

**Severity:** Medium - `ui/src/app-session-actions.ts:316-356`. `useAppSessionActions` now requires `latestStateRevisionRef`, `lastSeenServerInstanceIdRef`, `projectsRef`, and `sessionsRef` because of the inline `classifyRejectedActionState` call site. The ref count grew from 1 → 4 in a few rounds, all to feed one classifier function.

**Current behavior:**
- Every new evidence dimension for stale action snapshots pushes another ref into this hook.
- App.tsx, the test harness, and the hook signature all need editing whenever a new dimension is added.
- Same anti-pattern the resync-options ref cluster had before extraction.

**Proposal:**
- Pass a single `actionStateClassifierContextRef: MutableRefObject<{ revision, serverInstanceId, projects, sessions }>` (or a memoized snapshot getter) so adding a new evidence dimension does not require touching the hook signature, the caller, and the test harness.
- Defer to a dedicated commit per CLAUDE.md.

## `connectionRetryDisplayStateByMessageId` two-stage memoization is correct but threaded through ~4 stability hops

**Severity:** Medium - `ui/src/SessionPaneView.tsx:858-895`. The retry-display memoization now uses `signature → ref-cached map → useCallback wrapper → useSessionRenderCallbacks deps → MessageCard renderer identity`. The map identity stability invariant is load-bearing for `SessionBody` memoization but only documented sparsely. A future change to retry-display semantics needs to be threaded through ~4 separate stability hops.

**Current behavior:**
- Hand-rolled signature-stable memo bridges to a renderer that already had its own deps tax.
- Reviewers have flagged this as "complex invariant without nearby comments" several rounds in a row.

**Proposal:**
- Extract the signature-stable memo into a small `useStableMapBySignature` hook in a sibling utility module so the pattern is reusable and named.
- Or memoize directly on `(messages, status)` and accept one rebuild per message-list change — `MessageCard` is already memoized below the `SessionBody` memo gate.

## Directory-level state hardening retains a TOCTOU window after symlink check

**Severity:** Low - `src/persist.rs:146-149`. Round-15 carryover. `harden_local_state_directory_permissions` calls `reject_existing_state_directory_redirection_unix` (which uses `fs::symlink_metadata`), then `harden_local_state_permissions(path, 0o700)` — which uses path-based `fs::set_permissions` and `fs::metadata`, both of which follow symlinks. An attacker able to replace the directory between the two calls would get the chmod redirected through the symlink. The matching file path now uses `O_NOFOLLOW + fchmod`, but the directory path has not been migrated.

**Current behavior:**
- File-level chmod is symlink-safe (O_NOFOLLOW + fchmod).
- Directory-level chmod is not.
- Mitigated by Phase-1 single-user threat model (only the user controlling `~/` could plant the symlink).

**Proposal:**
- Open the directory with `O_DIRECTORY | O_NOFOLLOW`, then `fchmod` on the resulting fd; or use `fchmodat(AT_FDCWD, path, mode, AT_SYMLINK_NOFOLLOW)`.

## `AgentSessionPanel.test.tsx` new tests duplicate ~70 lines of harness setup

**Severity:** Low - `ui/src/panels/AgentSessionPanel.test.tsx:369-505`. The new "refreshes command/diff cards when only the renderer changes" tests replicate ~70 lines of harness setup each. Boilerplate increases drift risk.

**Proposal:**
- Extract a `renderAgentSessionPanelHarness` helper that takes only the props that vary (viewMode, message arrays, renderer overrides) and supplies the noop defaults internally.

## `App.live-state.reconnect.test.tsx` does not pin the "latest assistant message stays hidden until another action" invariant

**Severity:** Low - The closest coverage at lines 1505 and 1582 pins reconnect polling/disarm timing, but does not assert the visibility side. A regression that exposes the latest assistant message before SSE confirms could slip in.

**Proposal:**
- Add a test that dispatches a fallback `_sseFallback` snapshot containing only the user prompt (no assistant reply yet), confirms the assistant message is not shown, then either adopts a fresh SSE state event with the assistant text or simulates "another action" and asserts visibility.

## Non-optimistic user-prompt display causes 100-300ms felt lag on every Send

**Severity:** Medium - `ui/src/app-session-actions.ts:851-895` and `ui/src/app-live-state.ts:1283-1385`. The composer is non-optimistic: clicking Send clears the textarea, fires `await sendMessage(...)`, and then runs `adoptState(state)` against the full `StateResponse` returned by the POST. The "you said X" card only appears after the round-trip plus the heavy `adoptState` walk completes.

`adoptState` re-derives codex, agentReadiness, projects, orchestrators, workspaces, and walks transcripts on the main thread. On a focused active session this lands in the 100-300ms range every send (longer when an active turn is mid-stream). The codebase has already self-diagnosed the path in `docs/prompt-responsiveness-refactor-plan.md` but no optimistic-insert fix has landed.

The lag compounds with two existing tracked bugs ("Focused live sessions monopolize the main thread during state adoption", "Composer drafts have three authoritative stores") but is itself a separable contributor.

**Current behavior:**
- User clicks Send -> textarea clears -> POST fires -> response returns -> `adoptState` walks -> card paints.
- Total delay: round-trip (typically 30-100ms locally) + adoptState (50-200ms on focused live sessions) = visible 100-300ms gap.
- During the gap the session shows neither the user prompt nor the composer text.

**Proposal:**
- Insert an optimistic user-message card in `handleSend` before `await sendMessage(...)`, keyed by a temp id.
- When the POST response arrives or the SSE `messageCreated` delta lands (whichever is first), reconcile by id (swap temp id for server-assigned `messageId`).
- This collapses the round-trip and the adoptState walk out of the felt-lag path simultaneously.
- Cross-link to `docs/prompt-responsiveness-refactor-plan.md` and decide whether this is a standalone fix or folds into the larger refactor.

## `applyDeltaToSessions` duplicates the "lookup first, metadata-only fallback when missing" pattern across five non-created delta types

**Severity:** Low - `ui/src/live-updates.ts:329-599`. The reordered `messagesLoaded === false` branch (apply to in-memory message when present, fall back to metadata-only only when `messageIndex === -1`) is now repeated five times across `messageUpdated`, `textDelta`, `textReplace`, `commandUpdate`, and `parallelAgentsUpdate`. The previous code had the same shape duplicated five times in the wrong order; the new code is now the right order duplicated five times. A future sixth retained-non-created delta type will need to re-derive the same flow.

**Current behavior:**
- Each branch independently re-implements `findMessageIndex` -> `if (-1 && !messagesLoaded) metadata-only` -> `if (-1) needsResync` -> type-narrow -> apply.
- The existing duplication is what let the fallback land in the wrong order originally; the next protocol addition has the same cliff.

**Proposal:**
- Extract a `tryApplyMetadataOnlyFallbackForMissingTarget(session, sessionIndex, sessions, delta)` helper (or similar) that centralizes the missing-target/unhydrated decision so each delta type calls a single helper instead of inlining the same branch.
- Defer to a dedicated pure-code-move commit per CLAUDE.md.

## `live-updates.test.ts` "applies retained non-created deltas" bundles five delta types into one `it()` block

**Severity:** Low - `ui/src/live-updates.test.ts:1655-1888`. The new "applies retained non-created deltas while the transcript is marked unhydrated" test covers `messageUpdated`, `textDelta`, `textReplace`, `commandUpdate`, and `parallelAgentsUpdate` serially within a single `it(...)`. If an early assertion fails, downstream cases never run and the failure trace doesn't pinpoint which delta type regressed.

**Current behavior:**
- Five distinct scenarios share one `it` block with sequential `expect` blocks.
- The companion metadata-only-fallback test at lines 1890-1929 covers only `textDelta` for the missing-target path (already tracked as a P2 backlog item).

**Proposal:**
- Split into five `it(...)` blocks (one per delta type) or use `it.each(...)` with a table that mirrors the existing P2 missing-target task.
- Pure mechanical change.

## Production SQLite persistence is bypassed in the test build

**Severity:** Medium - `src/app_boot.rs:229`. The runtime persistence changes now depend on SQLite schema setup, startup load, metadata writes, per-session row updates, tombstone cleanup, and cached delta persistence, but `#[cfg(test)]` still routes the background persist worker through the old full-state JSON fallback.

Many production SQLite helpers in `src/persist.rs` are `#[cfg(not(test))]`, so existing persistence tests can pass while the real runtime SQLite write/load/delete behavior remains unexercised. The newest post-commit hardening policy (`verify_persist_commit_integrity`, fatal owner-only permission verification, cache invalidation reset, and fatal pre-transaction redirection checks) is part of that production-only surface.

**Current behavior:**
- Test builds bypass `persist_delta_via_cache` and related SQLite write paths.
- Production SQLite load/save helpers are mostly compiled out under `cargo test`.
- Current tests cover retry bookkeeping and legacy JSON fixtures, but not the runtime SQLite persistence contract or the post-commit hardening decisions.

**Proposal:**
- Make the SQLite persistence path testable under `cargo test` with temp database files.
- Add coverage for full snapshot save/load, delta upsert, metadata-only update, hidden/deleted session row removal, and startup load from SQLite.
- Add coverage for post-commit permission failures, cache invalidation reset, and fatal redirection/reparse checks.
- Keep legacy JSON fixture tests separate from production runtime persistence tests.

## `SessionPaneView.tsx` and `app-session-actions.ts` past architecture file-size thresholds

**Severity:** Low - `ui/src/SessionPaneView.tsx` is now 3,160 lines and `ui/src/app-session-actions.ts` is 1,968 lines, both past the architecture rubric §9 thresholds (~2,000 for TSX components, ~1,500 for utility modules). The round-11 extractions of `connection-retry.ts`, `app-live-state-resync-options.ts`, `session-hydration-adoption.ts`, and `SessionPaneView.render-callbacks.tsx`, plus the later `action-state-adoption.ts` split, reduced these files but left them over their respective thresholds.

The companion `app-live-state.ts` entry already exists; this captures the two related Phase-2 candidates that emerged after the round-11 splits.

**Current behavior:**
- `SessionPaneView.tsx` mixes pane orchestration with reconnect-card / waiting-indicator / retry-display orchestration.
- `app-session-actions.ts` still mixes action handlers with optimistic-update and adoption-outcome side-effect wiring.
- Both files now have natural extraction boundaries with their own existing direct unit-test coverage.

**Proposal:**
- Pure code move per CLAUDE.md, in dedicated split commits (one per file).
- For `SessionPaneView.tsx`: candidate is the reconnect-card / waiting-indicator computation cluster.
- For `app-session-actions.ts`: candidate is the optimistic-update + adoption-outcome side-effect cluster now that pure stale target evidence has moved out.

## `App.live-state.deltas.test.tsx` past 2,000-line review threshold

**Severity:** Low - `ui/src/App.live-state.deltas.test.tsx`. File is now 3,435 lines and 18 `it` blocks after this round's cross-instance regression coverage, well past the architecture rubric §9 ~2,000-line threshold for TSX files. The header already lists three sibling files split out (`reconnect`, `visibility`, `watchdog`), establishing the per-cluster split pattern.

The newest tests still cluster around hydration/restart races and cross-instance recovery, which is a coherent split boundary. Pure code move per CLAUDE.md.

**Current behavior:**
- Single test file mixes hydration races, watchdog resync, ignored deltas, orchestrator-only deltas, scroll/render coalescing, and resync-after-mismatch flows.
- 18 `it` blocks; the newest coverage adds another cross-instance state-adoption scenario.
- Per-cluster grep tax growing.

**Proposal:**
- Pure code move: extract the 4–5 hydration-focused tests into `ui/src/App.live-state.hydration.test.tsx`, mirroring the sibling-split pattern.
- Defer to a dedicated split commit; do not couple with feature changes.

## `app-live-state.ts` past 1,500-line review threshold for TypeScript utility modules

**Severity:** Low - `ui/src/app-live-state.ts`. File is now 2,435 lines after this round. The architecture rubric §9 sets a pragmatic ~1,500-line threshold for TypeScript utility modules. The hydration adoption helpers have moved out, but the module still mixes retry scheduling, profiling, JSON peek helpers, and the main state machine.

**Current behavior:**
- Single module mixes hydration matching, retry scheduling, profiling, JSON peek helpers, and the main state machine.
- Per-cluster grep tax growing with each round.

**Proposal:**
- Defer to a dedicated pure-code-move commit per CLAUDE.md.
- Extract `hydration-retention.ts` (or `session-hydration.ts`) containing `hydrationRetainedMessagesMatch`, `SESSION_HYDRATION_RETRY_DELAYS_MS`, `SessionHydrationTarget`, `SessionHydrationRequestContext`, and the matching unit tests.

## `AgentSessionPanel.test.tsx` past 5,000-line review threshold

**Severity:** Low - `ui/src/panels/AgentSessionPanel.test.tsx`. File is now 5,659 lines (+511 this round), past the project's review threshold for test files. The added blocks cluster naturally by concern — composer memo coverage, scroll-following coverage, ResizeObserver fixtures — and would extract cleanly into siblings without behavioral change.

The adjacent `App.live-state.*.test.tsx` split (April 20) is the precedent for per-cluster `.test.tsx` files. Per `CLAUDE.md`, splits must be pure code moves and live in their own commit.

**Current behavior:**
- Single `AgentSessionPanel.test.tsx` mixes composer, scroll, resize, and lifecycle clusters.
- Per-cluster grep tax growing with each replay-cache-adjacent feature round.

**Proposal:**
- Pure code move: extract into `AgentSessionPanel.composer.test.tsx`, `AgentSessionPanel.scroll.test.tsx`, `AgentSessionPanel.resize.test.tsx` (matching the App.live-state cluster shape).
- Defer to a dedicated split commit; do not couple with feature changes.


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

**Severity:** Low - `ui/src/panels/SourcePanel.tsx` grew from ~803 to 1119 lines in this round (+316). It is approaching but has not crossed the ~2,000-line scrutiny threshold. The new responsibility (rendered-Markdown commit pipeline orchestration: collect — resolve ranges — check overlap — reduce edits — re-emit with EOL style) is meaningfully separable from the existing source-buffer/save/rebase/compare orchestration. It has its own state (`hasRenderedMarkdownDraftActive`, `renderedMarkdownCommittersRef`), pure helpers already split into `markdown-commit-ranges`/`markdown-diff-segments`, and a clean parent-callback interface.

**Current behavior:**
- SourcePanel owns two distinct orchestration responsibilities in one component.

**Proposal:**
- No action this commit. Consider extracting a `useRenderedMarkdownDrafts(fileStateRef, editorValueRef, setEditorValueState, ...)` hook in a follow-up, owning `renderedMarkdownCommittersRef`, `hasRenderedMarkdownDraftActive`, `commitRenderedMarkdownDrafts`, `handleRenderedMarkdownSectionCommits`, and `handleRenderedMarkdownSectionDraftChange`.
- The hook would expose a small surface for SourcePanel to consume and keep the file under the scrutiny threshold.

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

**Severity:** Low - `ui/src/message-cards.tsx:607-628` replaced `useLayoutEffect` with `useEffect` + a `requestAnimationFrame` before `setIsActivated(true)` for the near-viewport fast-activation branch. The previous sync layout-effect path activated heavy content that was already in-viewport before paint, avoiding a placeholder — content height jump. The new path defers activation by at least one paint, so on initial mount near the viewport the user may now see the placeholder for one frame before the heavy content replaces it. The deleted comment specifically warned about this risk for virtualized callers.

**Current behavior:**
- `useEffect` + `requestAnimationFrame` defers activation by ~1 paint even when the card is already near viewport on mount.
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

**Severity:** Low - `ui/src/panels/VirtualizedConversationMessageList.tsx:426-432` converted the `prevIsActive !== isActive` render-time derived-state update into a post-commit `useEffect`. Under the previous pattern, a session switching from `isActive: false — true` flipped `setIsMeasuringPostActivation(true)` during render, so the first frame rendered the measuring shell with the correct `preferImmediateHeavyRender` value. The new effect defers that flip to after commit — the first paint of the newly-active session briefly shows `isMeasuringPostActivation: false`, flipping to the measurement shell only on the next render.

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

## Composer sizing double-resets on session switch

**Severity:** Low - `ui/src/panels/AgentSessionPanel.tsx:918-931` runs `resizeComposerInput(true)` synchronously inside a `useLayoutEffect` keyed on `[activeSessionId]`, and a following `useEffect` keyed on `[composerDraft]` schedules another resize via `requestAnimationFrame` on the same first render. The rAF resize is redundant because the synchronous one already measured the new metrics.

**Current behavior:**
- Layout effect resets cached sizing state and calls `resizeComposerInput(true)` synchronously.
- Draft effect schedules a second `requestAnimationFrame` resize on the same first render.
- First render of any newly-activated session does two resize passes instead of one.

**Proposal:**
- Track a "just-resized-synchronously" flag set in the layout effect and checked at the top of `scheduleComposerResize`, or gate the draft effect with a prev-draft ref so the "initial draft equals committed" case is a no-op.

## Duplicated `Session` projection types in `session-store.ts` and `session-slash-palette.ts`

**Severity:** Low - `ComposerSessionSnapshot` (`ui/src/session-store.ts:36-83`) and `SlashPaletteSession` (`ui/src/panels/session-slash-palette.ts:51-65`) each re-pick overlapping-but-non-identical field sets from `Session`. Three `Session`-like shapes now exist (`Session`, `ComposerSessionSnapshot`, `SlashPaletteSession`) with no compile-time check that additions to `Session` reach both projections — a new agent setting added to `Session` could silently default to `undefined` in consumers that read through either projection.

**Current behavior:**
- Both projection types declare field lists by hand.
- No `Pick<Session, ...>` derivation; nothing fails to compile when `Session` grows a new field.

**Proposal:**
- Derive both types via `Pick<Session, ...>`, or express `SlashPaletteSession` as `Omit<ComposerSessionSnapshot, ...>` where their field sets differ.
- Colocate the derivations in `session-store.ts` so the projection contract is visible in one place.

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

**Severity:** Medium - `estimateConversationMessageHeight` in `ui/src/panels/conversation-virtualization.ts` produces an initial height for unmeasured cards using a per-line pixel heuristic with line-count caps (`Math.min(outputLineCount, 14)` for `command`, `Math.min(diffLineCount, 20)` for `diff`) and overall ceilings of 1400/1500/1600/1800/900 px. For heavy messages — review-tool output, build logs, large patches — the estimate is 20–40% under the rendered height, so `layout.tops[index]` for cards below an under-priced neighbour places them inside the neighbour's rendered area. The user sees the cards painted on top of each other for one frame, until the `ResizeObserver` measurement lands and `setLayoutVersion` rebuilds the layout.

An initial attempt to fix this by raising estimates to a single 40k px cap (and adding `visibility: hidden` per-card until measured) was reverted after it introduced two worse regressions: (1) per-card `visibility: hidden` combined with the wrapper's `is-measuring-post-activation` hide left the whole transcript empty for a frame whenever the virtualization window shifted before measurements landed; (2) raising the cap made the `getAdjustedVirtualizedScrollTopForHeightChange` shrink-adjustment huge (40k estimate − 8k actual = −32k scrollTop jump), so slow wheel-scrolling through heavy transcripts caused visible scroll jumps of tens of thousands of pixels. The revert restores the one-frame overlap as the known limitation.

**Current behavior:**
- Initial layout uses estimates that badly under-price long commands / diffs.
- First paint places subsequent cards overlapping the under-priced one for one frame.
- Next frame, `ResizeObserver` fires, `setLayoutVersion` rebuilds, positions correct.
- Visible to the user as a brief "jumble" during scroll.

**Proposal:**
- Proper fix likely needs off-screen pre-measurement (render the card in a hidden measure-only tree, read `getBoundingClientRect` height, then place in the layout) rather than a formula-based estimate. This is a bigger change than a single pure-function tweak.
- Alternative: batch-measurement pass when the virtualization window shifts — hide the wrapper briefly, mount the newly-entering cards, wait for all their measurements, then reveal.
- Not: raise the estimator cap. Large overshoots trade one visible artifact for a worse one.


## Hard kill (SIGKILL, power loss) can still lose the last un-drained persist write

**Severity:** Low - restarting the backend process while the browser tab is still open can make the most recent assistant message disappear from the UI, because the persist thread has a small window between "commit fires" and "row is durably in SQLite" during which an un-drained mutation is lost on kill.

Persistence is intentionally background and best-effort: every `commit_persisted_delta_locked` (and similar delta-producing commit helpers) signals `PersistRequest::Delta` to the persist thread and returns. The thread then locks `inner`, builds the delta, and writes. If the backend process is killed (SIGKILL, laptop sleep wedge, crash, manual restart of the dev process) between the signal fire and the SQLite commit, the mutation is lost. Old pre-delta-persistence behavior had the same window — the persist channel carried a full-state clone — so this is not a regression introduced by the delta refactor, but the symptom is visible now because the reconnect adoption path applies the persisted state with `allowRevisionDowngrade: true`: the browser's in-memory copy of the just-streamed last message is replaced by the freshly loaded (older) backend state, making the message disappear from the UI.

The message is not hidden; it is genuinely gone from SQLite. No amount of frontend re-rendering will bring it back.

**Current behavior:**
- Active-turn deltas (e.g., streaming assistant text, `MessageCreated` at the end of a turn) commit through `commit_persisted_delta_locked`, which only signals the persist thread.
- The persist thread acquires `inner` briefly, collects the delta, and writes to SQLite.
- Between "signal sent" and "row written" there is a small time window (usually sub-millisecond, but can stretch under contention) during which a hard kill of the backend loses the mutation.
- On backend restart + SSE reconnect, the browser's `allowRevisionDowngrade: true` adoption path applies the persisted state. The persisted state is missing the un-drained mutation, so the in-memory latest message is overwritten and disappears.

**Proposal:**
- The user-initiated restart path (Ctrl+C / SIGTERM) is now covered by the graceful-shutdown drain — see the preamble.
- For the residual hard-kill case (SIGKILL, power loss): consider opt-in synchronous persistence for the last message of a turn — the turn-completion commit (`finish_turn_ok_if_runtime_matches`'s `commit_locked`) could flush synchronously before returning, trading a few ms of latency on turn completion for zero-loss durability of the final message.
- Or accept and document this as a known Phase-1 limitation in `docs/architecture.md` (background-persist durability contract: at most one un-drained mutation may be lost on hard kill).

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

- Replace the unbounded queue with a single-slot latest mailbox or bounded channel.
- Drop or overwrite superseded snapshots before they can accumulate in memory.
- Add a burst test that publishes multiple large snapshots while the broadcaster is delayed and asserts only the latest snapshot is retained.

## Reload persistence regression leaves a restarted worker alive during cleanup

**Severity:** Low - the reload test can produce noisy or flaky cleanup because the reloaded state's worker is not shut down.

The graceful-shutdown reload regression creates a fresh `AppState::new_with_paths` to verify persisted state, but then drops it without shutting down its persist worker before temp directories are removed. That worker can still enqueue boot-time persist work and log failed writes after the test deletes its temporary persistence path, which is especially fragile on Windows.

**Current behavior:**
- The test reloads state through a new `AppState`.
- The reloaded state's persist worker is not explicitly drained before temp cleanup.

**Proposal:**
- Call `restarted.shutdown_persist_blocking()` after dropping any held inner-state guards and before removing temp directories.
- Keep the test's durable-reload assertion intact while making cleanup deterministic.

## Generated Vitest cache file is modified under `node_modules`

**Severity:** Note - local test-runner cache churn can be accidentally staged.

The unstaged diff includes `node_modules/.vite/vitest/da39a3ee5e6b4b0d3255bfef95601890afd80709/results.json`, which is generated local Vitest state. If staged, it would add machine-specific noise unrelated to the product change.

**Current behavior:**
- A generated Vitest cache file is modified in the worktree.
- The file is not part of the application behavior under review.

**Proposal:**
- Leave the file unstaged or revert the generated cache change before committing.
- Consider ignoring the cache path if it is not intentionally tracked.

## Implementation Tasks

- [ ] P2: Add production SQLite persistence coverage:
  make the SQLite runtime persistence path available under `cargo test`, then cover temp-database full snapshot save/load, delta upsert, metadata-only update, hidden/deleted row removal, and startup load.
- [ ] P2: Add Windows state-path redirection coverage:
  cover SQLite main-file symlinks, sidecar symlinks, and `.termal` directory junction/symlink cases behind Windows-gated tests.
- [ ] P2: Add post-shutdown persistence ordering coverage:
  race a late background commit against `shutdown_persist_blocking()` and prove the final persisted state reflects the latest `StateInner`, not an older worker-drained delta.
- [ ] P2: Add concurrent shutdown idempotency race coverage:
  call `shutdown_persist_blocking()` concurrently from two `AppState` clones and assert `persist_worker_alive` cannot flip false until the join owner has returned.
- [ ] P2: Add graceful-shutdown open-SSE coverage:
  cover both shutdown-before-connect and shutdown-after-initial-state through `/api/events`, and assert the stream exits within a timeout so the persist drain is reached.
- [ ] P2: Add shutdown persist failure retry coverage:
  force the final shutdown persist attempt to fail once and then succeed, and assert the worker does not exit before the successful write.
- [ ] P2: Add no-worker shutdown idempotence assertions:
  assert `shutdown_persist_blocking()` leaves `persist_thread_handle` as `None` and does not send a `PersistRequest::Shutdown` when no worker handle exists.
- [ ] P2: Add route-level Lagged SSE wire-format coverage:
  force `RecvError::Lagged` through `/api/events` and assert the raw frame emits `event: lagged`, non-empty `data: 1`, then the recovery `state`.
- [ ] P2: Tighten send-after-restart live-stream assertion scope:
  the round-13 extension dispatches a delta on the recreated `EventSource` and asserts the recovered text appears, but uses page-wide `getAllByText` which the same delta's `preview` field can satisfy via the sidebar tooltip alone. Scope the assertion to `document.querySelector(".message-card.bubble-assistant")?.textContent`. Mirrors the tripwire test's scoping pattern.
- [ ] P2: Add non-send action restart live-stream delta-on-recreated-stream coverage:
  the round-13 fix proves `forceSseReconnect()` is called on cross-instance `adoptActionState` recovery, but does not dispatch live deltas through the recreated EventSource. Submit an approval/input-style action after backend restart, then dispatch assistant deltas on the new `EventSourceMock` and assert they render in the active transcript bubble.
- [ ] P2: Add remote `lagged` SSE bridge regression:
  feed `dispatch_remote_event` / the remote stream processor a `lagged` marker followed by a same-revision recovery `state`, and assert the remote snapshot is honored instead of ignored by revision gates.
- [ ] P2: Add reconnect force-adoption monotonicity regressions:
  prove lower same-instance `/api/state` watchdog snapshots are rejected, reconnect snapshots stay rejected, and a Lagged marker only force-adopts the following state while the client is still at the marker baseline.
- [ ] P2: Add failed manual retry reconnect-rearm regression:
  cover manual retry hitting a transient failure, then the next scheduled attempt adopting a newer same-instance snapshot while polling still continues until SSE confirms.
- [ ] P2 workspace-file bad-live-event recovery regression:
  dispatch a bad reopened SSE payload followed by `workspaceFilesChanged`, and assert reconnecting state plus fallback polling remain active.
- [ ] P2 watchdog wake-gap stop-after-progress regression:
  trigger watchdog wake-gap recovery, adopt same-instance `/api/state` progress, and assert no additional reconnect polling occurs before a later live event.
- [ ] P2: Add `forceSseReconnect` unit-level regression:
  drive `handleSend` with a server-instance-mismatched response in `app-session-actions.test.ts` and assert both `params.requestActionRecoveryResync` and `params.forceSseReconnect` are called. The mock prop already exists in `makeSessionActionsParams` but no triggering test exercises it; the existing app-level `EventSourceMock.instances.length >= 2` assertion proves composition end-to-end but a unit test would catch a future refactor that drops the `forceSseReconnect()` call.
- [ ] P2: Tighten `graceful_shutdown_drain_persists_final_mutation_across_reload` against worker timing:
  the current single-commit-then-shutdown shape can pass even without the graceful-drain fix if the worker happens to drain the Delta before Shutdown arrives. Commit many sessions in rapid succession (e.g. 50) before calling `shutdown_persist_blocking()` so at least one is guaranteed in-flight, and assert all of them are reloadable from the fresh `AppState::new_with_paths`.
- [ ] P2: Add reconnect-fallback polling-continuation assertion to `rejects a lower same-instance reconnect`:
  after asserting the lower-revision rejection, advance fake timers by `RECONNECT_STATE_RESYNC_DELAY_MS` and verify `fetchStateSpy` fires a second time. Without this, a regression that rejects rollback AND incorrectly clears the polling loop would still pass.
- [ ] P2: Add direct unit tests for the moved `markdown-diff-clipboard-pointer.ts` helpers:
  the new module's header explicitly markets the helpers as testable now that they have no React/component-tree dependencies, but no `markdown-diff-clipboard-pointer.test.ts` exists. Cover (a) `rangeCoversNodeContents` inclusive equality / range fully inside / range escaping start or end; (b) `serializeSelectedMarkdown`'s three fallbacks (round-trip wins; covers-whole-body fallback; `range.toString()` final fallback when round-trip yields empty); (c) `getSelectionRangeInsideSection`'s rejection of collapsed / no-selection / out-of-section ranges.
- [ ] P2: Cover the index clamp-on-shrink branch in `MarkdownDiffView` and `RenderedDiffView`:
  re-render the parent with a smaller `regions`/`segments` array while `currentChangeIndex`/`currentRegionIndex` points past the new end and assert the counter snaps to "Change/Region 1 of N" while prev/next still wrap correctly. Today the existing prev/next tests only exercise wrap-around at full length; the `current >= changeCount/regionCount` clamp branch in the `useEffect` is unexercised.
- [ ] P2: Make `DiffPanel.test.tsx` `scrollIntoView` mocking remount-resilient:
  the existing test pins `scrollIntoView` on the two block nodes captured via `waitFor`. A future change that remounts the `<section>` between clicks (e.g., a `key` churn) would silently stop firing the mock. Apply the mock via `Element.prototype.scrollIntoView = vi.fn()` in `beforeEach` (or re-query before each click) so any node — re-mounted or not — hits the same fn.
- [ ] P2: Pin additional boundary edges in the `markdown-streaming-split` round-trip invariant:
  add `"Hello\n"`, `"\n"`, `"\n\n"`, and `"$$\nx"` to the round-trip array. Add a math-display parity case "settles `$$...$$` followed by partial prose" matching the table/fence suite's existing closed-block-then-partial-prose case.
- [ ] P2: Add production-path streaming Markdown coverage:
  render active assistant `MessageCard` cases for partial pipe tables, standalone `$$` display math, and tilde fences, and assert the streaming placeholder path is actually reached.
- [ ] P2: Add CommonMark fence edge-case splitter coverage:
  cover a four-backtick opener containing triple backticks and a backtick fence containing `~~~`, then pin that only matching character/length closers settle the fence.
- [ ] P2: Add rendered diff render-budget coverage:
  create many Mermaid/math rendered regions and assert the preview applies the same document-level caps as a single `MarkdownContent` document.
- [ ] P2: Add single-target rendered diff navigation coverage:
  assert prev/next scrolls the only Markdown diff change and the only rendered diff region even though the selected index does not change.
