# Bugs & Known Issues

This file tracks only reproduced, current issues and open review follow-up
tasks. Resolved work, fixed-history notes, speculative refactors, cleanup notes,
and external limitations do not belong here. Review follow-up task items live in
the Implementation Tasks section.

## Active Repo Bugs

## Telegram settings updates live outside the app state/revision model

**Severity:** Medium - Telegram settings are user-visible configuration, but saves bypass `StateInner`, `commit_locked()`, snapshots, revisions, and SSE.

`src/telegram_settings.rs:30` updates `~/.termal/telegram-bot.json` directly through the Telegram settings endpoint. That means one browser tab can save config while other tabs keep stale settings until they manually refetch, and future relay lifecycle work will need to reconcile app state with a separate settings file.

**Current behavior:**
- Telegram settings updates do not bump the app revision.
- `/api/state` and SSE do not carry the changed config.
- Other open clients cannot observe settings changes through the normal state model.

**Proposal:**
- Store Telegram UI config in durable app state and mutate it through `commit_locked()`.
- If `telegram-bot.json` remains necessary for adapter interop, mirror committed state to that file behind a documented boundary.

## Telegram settings and relay state can overwrite each other in `telegram-bot.json`

**Severity:** Medium - the UI settings endpoint and Telegram relay both read-modify-write the same JSON file without a shared lock or safe merge protocol.

`src/telegram_settings.rs:34` and `src/telegram.rs:196` both update `telegram-bot.json`. Concurrent `/api/telegram/config` calls, or a transitional `cargo run -- telegram` relay persisting cursor state while the UI saves config, can lose either UI-owned token/config fields or runtime-owned `chatId` / `nextUpdateId` fields. `persist_telegram_bot_state` also drops read/parse failures with `.ok()`, so a malformed or temporarily unreadable settings file can be treated as default and rewritten without existing config.

**Current behavior:**
- Settings saves and relay state persistence share one file.
- Writes are read-modify-write operations without same-process or cross-process serialization.
- Relay persistence defaults on read/parse failure and can overwrite existing config.

**Proposal:**
- Split UI config and runtime cursor/chat state into separate files, or guard all writes with a shared lock plus atomic reload-before-merge.
- Default only on `NotFound`; otherwise propagate/log read/parse failures and avoid rewriting when existing config cannot be safely loaded.
- Add interleaving coverage proving config and runtime state both survive competing writes.

## Telegram bot token file is written without explicit secret permissions

**Severity:** Medium - the saved Telegram bot token is a durable remote-control credential whose on-disk protection depends on umask or inherited ACLs.

`src/telegram_settings.rs:39` accepts a bot token from the UI and persists it via plain `fs::write` to `~/.termal/telegram-bot.json`. The status response masks the token, but the file itself is not created or updated with explicit user-only permissions.

**Current behavior:**
- Bot tokens are stored in the local Telegram settings JSON file.
- Unix file mode depends on process umask.
- Windows file access depends on inherited directory ACLs.

**Proposal:**
- Prefer an OS secret store where available.
- At minimum create/update the file with explicit user-only permissions and document platform fallback behavior.
- Add a Unix regression that verifies restrictive permissions for the token file.

## Telegram test endpoint maps upstream failures to backend-unavailable

**Severity:** Medium - invalid Telegram tokens and Telegram API failures can surface as "The TermAl backend is unavailable" instead of an actionable Telegram error.

`src/telegram_settings.rs:75` returns `ApiError::bad_gateway` from `/api/telegram/test`, while `ui/src/api.ts:1651` maps all 502/503/504 responses to `backend-unavailable` and discards the server `{ error }` body.

**Current behavior:**
- Telegram connection-test upstream failures use a 502-style response.
- The frontend classifies that status family as TermAl backend unavailability.
- The Telegram-specific failure body is not shown to the user.

**Proposal:**
- Return a status that the frontend treats as `request-failed`, or make frontend error classification path-aware for `/api/telegram/test`.
- Add coverage for an invalid-token/upstream-failure response proving the UI shows the Telegram-specific message.

## Telegram preferences panel has no frontend coverage

**Severity:** Medium - the new settings UI owns async behavior and payload normalization without RTL coverage.

`ui/src/preferences-panels.tsx:1214` adds `TelegramPreferencesPanel`, including initial load, save, test-connection, token masking, project/session filtering, default-project auto-subscription, stale default-session clearing, and notice/error states. No frontend tests currently reference `TelegramPreferencesPanel`, `fetchTelegramStatus`, `updateTelegramConfig`, or `testTelegramConnection`.

**Current behavior:**
- The Telegram tab renders a new stateful settings workflow.
- Async load/save/test flows are untested.
- Payload normalization and saved-token behavior are unpinned.

**Proposal:**
- Add React Testing Library coverage for initial status load, save payload normalization, saved-token test flow, API error display, and the AppDialogs Telegram tab path.

## Telegram settings API leaks local paths in file-error responses

**Severity:** Low - settings read/write failures can expose the user profile path and TermAl data directory through API error messages.

`src/telegram_settings.rs:114` and related helpers include full settings paths in client-visible failures from `/api/telegram/status`, `/api/telegram/config`, and `/api/telegram/test`.

**Current behavior:**
- File read, parse, create, and write failures can include absolute local paths.
- The detailed message is returned to the browser/API caller.

**Proposal:**
- Log detailed filesystem paths server-side.
- Return generic client messages such as "failed to read Telegram settings" or "failed to save Telegram settings".

## Telegram JSON body rejections bypass the project `ApiError` envelope

**Severity:** Low - malformed Telegram settings request bodies can return Axum's default rejection shape instead of the project's `{ "error": ... }` contract.

`src/telegram_settings.rs:260` and the sibling Telegram JSON body endpoint use direct `Json<T>` extraction. Other request-body endpoints that need consistent API errors accept `Result<Json<_>, JsonRejection>` and convert through `api_json_rejection`.

**Current behavior:**
- Malformed `/api/telegram/config` or `/api/telegram/test` JSON is handled by Axum extraction rejection.
- Response shape can differ from the project `ApiError` JSON envelope.

**Proposal:**
- Mirror the existing `Result<Json<_>, JsonRejection>` pattern and convert malformed JSON through `api_json_rejection`.

## `TelegramStatusResponse.subscribedProjectIds` wire shape does not match TypeScript

**Severity:** Low - frontend callers can trust a required array that Rust omits when it is empty.

`ui/src/api.ts:625` marks `TelegramStatusResponse.subscribedProjectIds` as required, but `src/wire.rs:1041` skips serializing the field when the vector is empty.

**Current behavior:**
- Rust omits `subscribedProjectIds` for an empty subscription list.
- TypeScript declares the field as always present.
- Future callers may crash or skip fallback logic by trusting the type.

**Proposal:**
- Always serialize an empty array from Rust, or mark the frontend field optional and normalize to `[]` at the API boundary.

## Telegram routes and tail-session hydration are missing from the architecture endpoint table

**Severity:** Low - newly implemented client-visible API behavior is not reflected in the central REST endpoint documentation.

`src/main.rs:233` registers `/api/telegram/status`, `/api/telegram/config`, and `/api/telegram/test`, but `docs/architecture.md` does not list these routes in its endpoint table. The same table still describes `GET /api/sessions/{id}` as a full-session fetch even though the new `?tail=N` query can intentionally return a partial local transcript with `messagesLoaded: false`.

**Current behavior:**
- The feature brief mentions Telegram endpoints.
- The architecture REST table omits methods, status/error semantics, and response shapes for the implemented Telegram routes.
- The session-fetch contract does not document `tail`, the tail cap/semantics, or the partial-response shape.

**Proposal:**
- Add the Telegram routes to `docs/architecture.md` with methods, request/response shapes, and error semantics.
- Document `GET /api/sessions/{id}?tail=N`, including when `messagesLoaded` is false and why callers must treat that response as a tail window rather than a full transcript.

## Telegram preferences save/test handlers set state after awaited requests without an unmount guard

**Severity:** Low - closing the settings dialog or switching away while a request is in flight can run React state updates after the panel unmounts.

`ui/src/preferences-panels.tsx:1322` and the test-connection handler update `status`, `draft`, `notice`, `error`, `isSaving`, and `isTesting` after awaited API calls. The initial load effect has a cancellation guard, but the user-triggered save/test flows do not.

**Current behavior:**
- `handleSave` and `handleTestConnection` await network calls.
- Post-await state updates are unconditional.
- The component can unmount while the request is still pending.

**Proposal:**
- Track mounted state or use abortable requests and guard every post-await state update in both handlers.

## Telegram settings UI belongs behind a focused module boundary

**Severity:** Low - the Telegram panel adds a large independent API workflow to the already broad preferences panel module.

`ui/src/preferences-panels.tsx:1214` adds several hundred lines of Telegram settings state, effects, API calls, payload shaping, and rendering to a file that already owns multiple preferences panels.

**Current behavior:**
- Telegram settings lifecycle/config UI lives inside `preferences-panels.tsx`.
- Fetch/save/test behavior and render structure are coupled to the broad preferences module.

**Proposal:**
- Extract Telegram settings UI and its fetch/save/test hook into a dedicated preferences or telegram-settings module.

## `UpdateTelegramConfigRequest` `Option<Option<String>>` cannot distinguish absent from null without `deserialize_with`

**Severity:** High - bot_token, default_project_id, and default_session_id can never be cleared via `POST /api/telegram/config` because serde collapses both absent fields and `null` to outer `None`.

`src/wire.rs:1052-1058`. The `Option<Option<String>>` shape is meant to express PATCH semantics ("field absent = don't update; field null = clear"), but without `#[serde(default, deserialize_with = "deserialize_nullable_marker_field")]` (the helper used for analogous marker fields at lines 393-401, 421-446 in the same file), serde treats both shapes identically. The backend's `if let Some(bot_token) = request.bot_token { ... }` arm therefore never fires for the clear case. The current Telegram preferences save flow sends `defaultProjectId: null` / `defaultSessionId: null` when clearing those fields (`ui/src/preferences-panels.tsx:1334`), so choosing "No default project/session" can appear to save but leave the old persisted values in place. `UpdateTelegramConfigPayload` in `ui/src/api.ts:632` also documents `botToken?: string | null`, so the same dead-letter path affects future token-clearing UI.

**Current behavior:**
- `Option<Option<String>>` fields use the default serde derivation.
- `null` and absent JSON fields both deserialize to outer `None`.
- The match arm that would clear the stored value never fires.
- The current UI sends `null` to clear default project/session selections, so those values can remain stuck.

**Proposal:**
- Add `#[serde(default, deserialize_with = "deserialize_nullable_marker_field")]` to each `Option<Option<T>>` field on `UpdateTelegramConfigRequest`, mirroring `UpdateConversationMarkerRequest`'s `body`/`end_message_id`.
- Or replace the `Option<Option<_>>` ladder with an explicit discriminated request (`bot_token: Option<TokenUpdate>` where `TokenUpdate::Clear`/`TokenUpdate::Set(String)`).
- Add coverage where `null` is sent and the persisted value is cleared.

## `src/telegram_settings.rs` ships with zero Rust tests for the validation gatekeeper and read-path sanitizer

**Severity:** High - `validate_telegram_config` (auto-subscribe of `default_project_id`, auto-fill of `default_project_id` from `default_session_id`, cross-project session rejection) and `sanitize_telegram_config_for_current_state` (strip stale references on read) are entirely uncovered.

`src/telegram_settings.rs:138-208 validate_telegram_config` and `:210-248 sanitize_telegram_config_for_current_state`. These are the gatekeepers for every persisted Telegram-relay config and the read-path safety net that protects every UI render against stale references. A subtle regression — flipping a comparison on lines 184-194, dropping the auto-subscribe at line 168, negating the retain check on lines 226-228 — would not be caught. The save flow in `TelegramPreferencesPanel.handleSave` already pre-includes `defaultProjectId` in `nextProjectIds`, so a backend regression that quietly clears subscriptions cannot be observed through UI tests either.

**Current behavior:**
- `validate_telegram_config` has no tests covering auto-subscribe, auto-fill, unknown project rejection, unknown session rejection, session-belongs-to-different-project rejection, or no-project-on-session rejection.
- `sanitize_telegram_config_for_current_state` has no tests for the cross-project default-session clearing branch.
- `mask_telegram_bot_token`, `normalize_project_id_list`, `normalize_optional_id`, `normalize_optional_secret` are all uncovered pure functions.
- The three HTTP route handlers (`get_telegram_status`, `update_telegram_config`, `test_telegram_connection`) have no router-level integration test.

**Proposal:**
- Add `src/tests/telegram_settings.rs` with focused cases for each branch in `validate_telegram_config` and `sanitize_telegram_config_for_current_state`.
- Add unit tests for `mask_telegram_bot_token` (empty, all-whitespace, < 8 chars, unicode, full-length token) and `normalize_project_id_list` (dedup with whitespace, empty filtering).
- Add a router-level test that hits each new `/api/telegram/...` route and asserts JSON shape.

## Inconsistent mutex error handling in `src/telegram_settings.rs` deviates from project convention

**Severity:** Medium - the new module uses `lock().map_err(|_| ApiError::internal("state lock poisoned"))?` and a silent `let Ok(inner) = ... else { return config; }` while every other file in `src/` (50+ call sites in delegations.rs, app_boot.rs, codex_submissions.rs, paths.rs, etc.) uses the documented `expect("state mutex poisoned")` pattern.

`src/telegram_settings.rs:142 validate_telegram_config` uses `map_err`; `:214 sanitize_telegram_config_for_current_state` uses silent fallback. Project convention (CLAUDE.md, accepted-patterns list) is `expect("state mutex poisoned")` so a poisoned mutex aborts the process and supervision restarts. Catching the poison and returning a 500 means the next request silently sees corrupted state. The two patterns within one module also disagree: a poisoned-mutex event would 500 on `update_telegram_config` but silently return unsanitized config on the read path.

**Current behavior:**
- `validate_telegram_config` returns `ApiError::internal("state lock poisoned")` on poisoned mutex.
- `sanitize_telegram_config_for_current_state` silently returns the unmodified config.
- Both diverge from the project's documented `expect("state mutex poisoned")` pattern.

**Proposal:**
- Replace both with `let inner = self.inner.lock().expect("state mutex poisoned");` to match project convention.
- Or document explicitly why this module intentionally diverges.

## `delete_project` does not prune Telegram config; persisted file accumulates stale project/session ids

**Severity:** Medium - when a project is deleted, `subscribed_project_ids` / `default_project_id` / `default_session_id` referencing that project remain in `~/.termal/telegram-bot.json`. The read path masks them via `sanitize_telegram_config_for_current_state`, but the file is never rewritten.

`src/session_crud.rs:478-523 delete_project` and `src/telegram_settings.rs:210-248`. A project re-created later with the same id would silently re-subscribe Telegram. The relay loop in `src/telegram.rs` reads the unsanitized list directly. The masked-on-read approach also produces protocol-level dishonesty: `TelegramStatusResponse.subscribedProjectIds` shown to the client may not match what's in the saved file.

**Current behavior:**
- Project deletion does not touch `~/.termal/telegram-bot.json`.
- Read-time `sanitize_telegram_config_for_current_state` masks the stale ids on GET, but persisted state remains stale.
- Relay loop reads the persisted unsanitized ids.

**Proposal:**
- `delete_project` should call into the Telegram-settings module to prune the deleted project id (and its sessions) from the saved config so on-disk state stays canonical.
- Or sanitize and persist on every read (paying an extra write per status fetch).
- Add coverage proving project deletion prunes Telegram subscriptions on disk.

## `useInitialActiveTranscriptMessages` mutates a ref during render

**Severity:** Medium - the new long-session tail-window hook writes `hydrationRef.current.sessionId` and `hydrationRef.current.hydrated = true` during render, breaking React 18 Strict Mode / concurrent rendering invariants.

`ui/src/panels/AgentSessionPanel.tsx:236-285`. The hook is part of the long-session tail-window path that activates only on transcripts above ~512 messages. Concurrent renders can flip `hydrated: true` before the actual commit, causing the windowing optimization to be skipped on first paint of large sessions. Worse, the second render re-keys the ref, potentially losing the "I started hydrating" intent. Most hooks in `panels/` use `useState` for derived-from-prop state with explicit reset effects.

**Current behavior:**
- `if (hydrationRef.current.sessionId !== sessionId) { hydrationRef.current = { hydrated: false, sessionId }; }` mutates during render (line 242-247).
- `if (!isTailEligible && messages.length > INITIAL_ACTIVE_TRANSCRIPT_TAIL_MIN_MESSAGES) { hydrationRef.current.hydrated = true; }` mutates during render (line 255-257).
- Strict Mode double-invoke fires the mutation twice without committing.

**Proposal:**
- Convert to `useState` with `useEffect` reset.
- Or use the React-docs "derived state" pattern: `const [prevSessionId, setPrev] = useState(sessionId); if (prevSessionId !== sessionId) { setPrev(sessionId); setHydrated(false); }`.
- Add Strict Mode coverage proving the windowing path still activates after a double-render.

## Active-transcript tail-window hook overlaps with `VirtualizedConversationMessageList`'s bottom-mount path

**Severity:** Medium - two layers (panel + virtualizer) gate "skip work for the tail" with different thresholds and different effects on dependent UI.

`ui/src/panels/AgentSessionPanel.tsx:175-286 useInitialActiveTranscriptMessages` windows messages to the last 96 before passing them to `ConversationMessageList` → `VirtualizedConversationMessageList`. The virtualizer's `preferInitialEstimatedBottomViewport` (round 53 addition) mounts the bottom range without rendering all messages above. The hook drops messages from React's perspective entirely (so `messageCount` becomes 0 → overview rail hides via `messageCount: isInitialTranscriptWindowActive ? 0 : visibleMessages.length` at line 804), while the virtualizer would just not mount unused slabs. A future reader changing the threshold has two places to keep in sync.

**Current behavior:**
- Hook drops messages above a 512-message session threshold, returning a 96-message tail.
- Virtualizer mounts only the bottom-of-viewport range via `preferInitialEstimatedBottomViewport`.
- Overview-rail gating uses `messageCount: 0` when the hook is windowing, hiding the rail.

**Proposal:**
- Move all "long session initial mount" logic into the virtualizer alone, then drop the hook.
- Or document the layer split with a header comment naming which problem each layer owns and why two layers exist.

## `update_telegram_config` validation and persist not atomic; concurrent calls produce stale-write last-writer-wins race

**Severity:** Medium - even within the backend process, two concurrent `POST /api/telegram/config` requests can both validate against the same `inner` snapshot, both load the same file copy, both validate independently, and the second write overwrites the first.

`src/telegram_settings.rs:34-56 update_telegram_config`. `validate_telegram_config` takes the state mutex briefly, releases it, then `persist_telegram_bot_file(&file)?` writes. Independent of the cross-process race documented in the existing "Telegram settings and relay state can overwrite each other" entry, this intra-process race can occur even when the standalone CLI relay isn't running. Symptoms: enabled-toggle flapping, subscription-list overwrites, default-project changes silently dropped under concurrent UI saves.

**Current behavior:**
- `validate_telegram_config` releases the state lock before `persist_telegram_bot_file` runs.
- Two concurrent HTTP updates can interleave validation and writes.
- No backend coordination between the two requests.

**Proposal:**
- Take a process-wide async mutex (e.g., `tokio::sync::Mutex` shared via `AppState`) covering load → mutate → validate → persist.
- Or acquire a file lock around the read-modify-write window (paired with the cross-process fix).
- Add coverage that drives two overlapping `update_telegram_config` calls and asserts the second observes the first's write.

## Telegram settings HTTP API split across three routes diverges from `/api/settings` convention

**Severity:** Medium - every other settings surface uses `POST /api/settings` returning `StateResponse` with SSE broadcast; Telegram uses `GET /api/telegram/status` + `POST /api/telegram/config` + `POST /api/telegram/test` returning `TelegramStatusResponse` with no broadcast.

`src/main.rs:233-235`. The `/test` route reasonably stays separate (genuinely a side-effecting outbound call). But splitting the GET/POST status+config into its own route is a divergence from the established pattern. The split also means none of the rest of the codebase's settings infrastructure (revision bumping, SSE broadcast, partial-payload merging via `UpdateAppSettingsRequest`) applies. A future caller scripting via the API has two patterns to learn.

**Current behavior:**
- Existing settings flow through `POST /api/settings` returning `StateResponse` (broadcast via SSE).
- Telegram settings use three new routes returning custom `TelegramStatusResponse` (not broadcast).
- The divergence is unexplained in code or docs.

**Proposal:**
- Fold the Telegram config bag into `UpdateAppSettingsRequest` with a `telegram: Option<UpdateTelegramConfigRequest>` field, returning `StateResponse` like every other setting.
- Or document explicitly in `docs/features/` why Telegram is intentionally separated (e.g., "secret tokens kept out of the broadcast snapshot").

## `mask_telegram_bot_token` reveals 8 of ~35-46 chars (~17-23% of secret)

**Severity:** Medium - industry convention (GitHub, Stripe, AWS) is at most 4 chars of suffix. `****<last 8>` is over-disclosed and the masked value renders in DOM as a placeholder, visible to shoulder surfers, screenshots, and screen-share recordings.

`src/telegram_settings.rs:299-308`. Telegram bot tokens are `<bot_id>:<35-char_secret>` (e.g., `1234567890:ABCdefGHIjklMNOpqrstUVwxYZ012345678`). Eight trailing characters cover 8 chars of the secret half. By itself this does not enable token recovery via brute-force, but the masked value is rendered in the preferences panel `<input>` placeholder (`ui/src/preferences-panels.tsx:1391`), making it visible in any screenshot or screen share of the settings dialog.

**Current behavior:**
- Masking shows `****<last 8 chars>` of the saved token.
- The masked value is rendered in `<input placeholder>`.
- Industry practice (GitHub, Stripe, AWS) shows 4 or fewer chars.

**Proposal:**
- Reduce suffix to 4 characters (`****<last 4>`).
- Or show only the public `<bot_id>` prefix (the part before `:`) which is non-secret.
- If "user can verify which token they configured" is the only goal, the last 4 chars are sufficient.

## `POST /api/telegram/test` is non-idempotent, has no rate limit, and falls back to saved-on-disk token

**Severity:** Medium - a script or wrapper can fan out parallel POSTs against api.telegram.org with no concurrency guard or per-token cooldown. The saved-token fallback also enables verify-by-side-effect (200 vs 502) of the saved token without knowing it.

`src/telegram_settings.rs:58-85`. Phase 1 single-user local mitigates the practical risk, but the route is the only Telegram endpoint that can fail with an outbound network error (502 `bad_gateway`), making it the most exposed surface on this endpoint family. The fallback path means a caller who only knows the route URL but not the token can still confirm whether the saved token is valid.

**Current behavior:**
- No per-process semaphore or token-bucket on the test endpoint.
- The saved-token fallback fires when no body token is supplied.
- Repeated calls fan out to api.telegram.org with no rate limit.

**Proposal:**
- Add a per-process semaphore (one in-flight call at a time, max N per minute).
- Reduce the saved-token fallback to "only when the request body explicitly opts in" via `useStoredToken: true`.
- Both are preventive; not blocking for Phase 1.

## `load_telegram_bot_file` silent fallback in `test_telegram_connection` masks parse failures

**Severity:** Medium - a corrupt/unparseable settings file silently degrades the test endpoint to "Telegram bot token is required" rather than surfacing the parse failure. `update_telegram_config` propagates the parse error correctly, so error-disclosure shape differs across two adjacent endpoints.

`src/telegram_settings.rs:62-69`. The `.ok()` swallow of `load_telegram_bot_file` failures means a user who can't run a connection test sees a misleading error. They then try to re-enter the token via `update_telegram_config`, which DOES propagate the parse error, surfacing the underlying issue. Two adjacent endpoints in the same module produce different error-disclosure shapes for the same underlying failure.

**Current behavior:**
- `test_telegram_connection` calls `load_telegram_bot_file().ok()` and silently treats failures as no-token.
- `update_telegram_config` propagates parse failures via `?`.
- Test endpoint surfaces "token is required" even when the saved file is corrupt.

**Proposal:**
- Propagate the load failure in `test_telegram_connection` (collapse to "could not read Telegram settings" rather than silently falling back).
- Or document the asymmetric fallback contract in the file header.

## `ConversationOverviewRail` roving tabindex stuck on the first segment

**Severity:** Medium - keyboard focus continuity is broken. `tabIndex={index === 0 ? 0 : -1}` is purely a function of the rendered index, not of "currently focused". After arrow-key navigation focuses a non-first segment, Tab-leaving and Tab-returning resets focus to the first segment.

`ui/src/panels/ConversationOverviewRail.tsx:288`. The standard roving tabindex pattern requires the focused element to carry `tabIndex=0` while all others carry `-1`. As implemented, the rail does not track which segment is focused, so Tab-out-and-back loses position. Combined with the documented arrow-key navigation, this is a regression for keyboard users navigating long transcripts.

**Current behavior:**
- Only segment index 0 carries `tabIndex=0`; all others carry `-1`.
- Arrow-key navigation moves focus but doesn't update tabIndex.
- Tab-out-and-back returns focus to segment 0, not the last interacted segment.

**Proposal:**
- Track `focusedSegmentIndex` in state, set it on segment focus, render `tabIndex={index === focusedSegmentIndex ? 0 : -1}`.
- Add coverage that focuses a non-first segment, blurs the rail, refocuses, and asserts focus restores to the previously-interacted segment.

## `ConversationOverviewRail` `event.preventDefault()` in compact-mode pointerdown blocks default focus

**Severity:** Medium - in compact mode the outer rail has `tabIndex={0}` to enable keyboard navigation, but `handleRailPointerDown` calls `event.preventDefault()` unconditionally, suppressing the default focus assignment on pointerdown. Click-then-arrow-keys interaction is broken; the user must Tab to the rail first.

`ui/src/panels/ConversationOverviewRail.tsx:115-131`. The preventDefault is needed to suppress text selection during drag, but it also blocks the browser from moving focus to the rail. Clicking on the compact rail navigates to the chosen segment but leaves focus elsewhere; subsequent arrow keys do not reach `handleCompactRailKeyDown`.

**Current behavior:**
- Rail has `tabIndex={0}` in compact mode.
- `handleRailPointerDown` calls `event.preventDefault()` unconditionally.
- Click does not move focus to the rail; arrow keys after click don't navigate.

**Proposal:**
- Conditionally call `event.preventDefault()` only when not in compact mode.
- Or explicitly call `event.currentTarget.focus()` after preventDefault in compact mode.
- Add coverage that clicks the compact rail and asserts subsequent ArrowDown navigates.

## Compact `ConversationOverviewRail` hides operable semantics behind a navigation landmark

**Severity:** Low - compact mode makes the rail keyboard-focusable and operable, but screen readers only receive a `navigation` landmark with hidden segment semantics.

`ui/src/panels/ConversationOverviewRail.tsx:235`. Once the overview crosses the compact threshold, per-segment buttons collapse into a single focusable rail. Keyboard handlers still make it an interactive control, but the element keeps `role="navigation"` and no equivalent current-position or segment-label semantics are exposed.

**Current behavior:**
- Compact mode has keyboard behavior on the outer rail.
- Segment semantics are no longer represented as individual buttons.
- Assistive tech sees a navigation landmark rather than an operable widget with position/selection state.

**Proposal:**
- Model compact mode with an appropriate widget role and ARIA state, or preserve accessible reduced segment buttons while using the compact visual track.
- Add accessibility-focused coverage for compact mode role/name/state.

## `validate_telegram_config` does TOCTOU between in-memory validation and on-disk persistence

**Severity:** Low - the validation reads `inner.projects` and `inner.sessions` while holding the state mutex, releases the lock, then `persist_telegram_bot_file(&file)?` writes. Between release and write, another thread could delete the validated project, leaving a persisted config that references a now-missing project.

`src/telegram_settings.rs:138-208`. The lock is correctly NOT held across I/O — that's the right call — but the TOCTOU window means the next status fetch will silently strip the dropped project ID via `sanitize_telegram_config_for_current_state`, which can be surprising to the user who just clicked Save. The read-time sanitize covers the symptom but not the underlying inconsistency.

**Current behavior:**
- Validation acquires the mutex briefly, then drops it.
- Persistence runs without holding the mutex.
- A concurrent project deletion between validation and persistence persists a stale reference.

**Proposal:**
- Add a header comment explaining the TOCTOU model and the sanitize-on-read recovery path.
- Or run `sanitize_telegram_config_for_current_state` after `validate_telegram_config` so the persisted file matches what the next read would return.

## `TelegramPreferencesPanel` does not memoize handlers, diverging from sibling preference panels

**Severity:** Low - `projectOptions` and `sessionOptions` are memoed, but `updateDraft`, `toggleProject`, `handleSave`, `handleTestConnection`, and the inline `onChange` lambdas at lines 1797, 1822, 1834 are recreated on every render. The two `ThemedCombobox` controls receive new function identity on every keystroke. Pattern divergence with `RemotePreferencesPanel` and other sibling panels in the same file.

`ui/src/preferences-panels.tsx:1214-1971`. A future reader copy-pasting from one panel to another now has two patterns to choose from.

**Current behavior:**
- Handlers are recreated on every render.
- Sibling preference panels in the same file memoize handlers.
- ThemedCombobox children receive new identity on every keystroke.

**Proposal:**
- Stabilize handlers via `useCallback`.
- Or document explicitly that the panel intentionally avoids memoization. Either is fine; consistency is the architectural goal.

## `UpdateTelegramConfigRequest` `Option<Option<String>>` PATCH-tristate pattern lacks doc comment

**Severity:** Low - the double-Option pattern is non-obvious, load-bearing, and inconsistent with every other PATCH-style endpoint in `wire.rs`.

`src/wire.rs:1052-1058`. The pattern is supposed to distinguish "field absent" from "field null" (see the High-severity entry above for why it doesn't actually work without `deserialize_with`). Even after that fix, a reader scanning the file will see `Option<Option<String>>` without explanation. A reader could simplify to single `Option` and silently break the PATCH semantics.

**Current behavior:**
- Three fields use `Option<Option<String>>` with no inline doc comment.
- Every other PATCH endpoint in `wire.rs` uses single `Option`.
- The pattern's PATCH semantics are not self-documenting.

**Proposal:**
- Add a `///` comment to each `Option<Option<...>>` field explaining the PATCH semantics.
- Or define a `PatchField<T>` newtype for clarity.

## `src/telegram_settings.rs` module header doesn't enumerate critical invariants

**Severity:** Low - the header explains the file format transition but does not document the two-writer race, validation TOCTOU, divergent lock-error handling, or sanitize-on-read recovery model.

`src/telegram_settings.rs:1-9`. The header describes "the relay loop still reads the legacy flat runtime fields … the file format below keeps those fields flat and adds a `config` object", but does not document: (a) the two-writer race with the standalone CLI relay, (b) the validation TOCTOU window, (c) why the lock-error handling diverges from project convention, or (d) the sanitize-on-read recovery model. This is the entry point for the next reader who needs to extend the module (e.g., the Phase 1 in-process relay lifecycle).

**Current behavior:**
- Header enumerates the file-format transition but no invariants.
- Future readers risk regressing the implicit contracts.

**Proposal:**
- Extend the header to enumerate (a) what owns what in the file, (b) coordination assumptions between writers, (c) lock-failure / IO-failure recovery model.

## `persist_telegram_bot_state` reads-then-writes the file unconditionally on every state change

**Severity:** Low - the relay polls every `TELEGRAM_DEFAULT_POLL_TIMEOUT_SECS` (5s default) and writes whenever `dirty`. The new logic adds a `fs::read` + `serde_json::from_slice` round-trip on every persist, doubling syscalls.

`src/telegram.rs:190-205`. Modest cost on its own. More concerning: if the file is concurrently being rewritten by the HTTP route, `fs::read` could observe a partial write (since `fs::write` truncates and rewrites without atomicity), and the relay would silently `unwrap_or_default()` — meaning a corrupt-read is treated as "first ever persist" and the next write erases the `config` portion. Pairs with the existing "Telegram settings and relay state can overwrite each other" entry.

**Current behavior:**
- Each persist does `fs::read` + parse + merge + `fs::write`.
- A partial-read mid-concurrent-write silently degrades to defaults.
- The next write erases legitimate config.

**Proposal:**
- Combine with the atomic-write fix on the existing two-writer-race entry.
- Distinguish "file does not exist" (legitimate first-run) from "file exists but unparseable mid-write" (warn + retry).

## `validate_telegram_config` mutates `&mut TelegramUiConfig` despite the "validate" name

**Severity:** Low - the function name implies a read-only check, but it auto-fills `default_project_id` from a session and auto-pushes the inferred project id into `subscribed_project_ids`. A reader of the call site `self.validate_telegram_config(&mut file.config)?;` has to remember that it also normalizes/back-fills.

`src/telegram_settings.rs:172-205`. Re-running validation on already-validated data is not idempotent if `default_project_id` was just inferred. Also pairs with the next entry: the inferred `session_project_id` is not re-validated against `known_projects`, so an orphan session record can backfill an orphan project id.

**Current behavior:**
- `validate_*` name implies pure check, but the function rewrites the config.
- Auto-subscribe and auto-fill are silent side effects.

**Proposal:**
- Rename to `validate_and_normalize_telegram_config` (or split into `normalize_telegram_config` + `validate_telegram_config`).
- Or move auto-push/back-fill into `update_telegram_config` immediately before the validation call, leaving validation read-only.

## `validate_telegram_config` doesn't re-validate auto-filled `session_project_id` against known projects

**Severity:** Low - when a `default_session_id` is set without a `default_project_id`, the auto-filled `session_project_id` is not re-checked against `known_projects` before being assigned to `default_project_id`.

`src/telegram_settings.rs:184-194`. If a session record references a project that has been deleted (e.g., a stale session record where `inner.projects` has lost the entry), the validator silently writes that orphan project id into `config.default_project_id`. The next `sanitize_telegram_config_for_current_state` call (on read) clears it again, so the user sees inconsistent persisted-vs-displayed state.

**Current behavior:**
- Session lookup returns a `session.project_id`.
- The id is assigned to `default_project_id` without `known_projects.contains(...)` re-check.
- `sanitize_*` later strips the orphan on read, masking the inconsistency.

**Proposal:**
- After resolving `session_project_id`, run the same `known_projects.contains(...)` check before assigning.
- Or eliminate the auto-fill entirely and reject the request with `default_project_id is required when default_session_id is set` so the UI is in charge of consistency.

## `TelegramUiConfig` derives `Debug` while holding the bot token in plaintext

**Severity:** Low - latent leakage risk. Today no code path Debug-formats a `TelegramUiConfig` value, but anyone who later adds `tracing::debug!("config = {config:?}")` or `.expect(format!("invalid config: {config:?}"))` would silently log the plaintext token.

`src/wire.rs:1014-1027`. The existing `TelegramApiClient` and `TelegramBotConfig` (both in `src/telegram.rs`) deliberately do NOT derive `Debug` for the same reason — this new struct breaks that pattern. The same applies to `TelegramBotFile` in `src/telegram_settings.rs:11-18` which contains `TelegramUiConfig` and would transitively expose it.

**Current behavior:**
- `TelegramUiConfig` derives `Debug`.
- Token is held in `bot_token: Option<String>`.
- Any future Debug-format would log plaintext.

**Proposal:**
- Remove `Debug` from the derive list.
- Or implement `Debug` manually with a redacted `bot_token` field (e.g., `f.debug_struct("TelegramUiConfig").field("bot_token", &"<redacted>")...`).

## `redact_telegram_bot_tokens` only matches `/bot<TOKEN>/` URL pattern; future formats slip through

**Severity:** Low - the redactor relies on the literal `/bot` substring followed by `/`. Catches the standard reqwest URL leak but misses any future error format that prints the token in another shape.

`src/telegram.rs:228-240`. Examples of misses: a `Display` impl that quotes the token, an HTTP-redirect chain to a path that doesn't include `/bot`, a debug log of headers if `Authorization`-style headers were ever introduced, or a panic message that includes the constructed `api_base_url` field outside a URL context.

**Current behavior:**
- Substring filter on `/bot<TOKEN>/` catches reqwest URL leaks.
- Other token-bearing log paths are not covered.
- The implementation depends on Telegram tokens never containing `/`.

**Proposal:**
- Complement with a value-based redactor that strips any substring matching the bot-token regex (`\d+:[A-Za-z0-9_-]{30,}`).
- Document the contract in a comment so a future reviewer knows the substring filter is the load-bearing leak guard.

## No length validation on `bot_token`, `default_project_id`, `default_session_id`, or `subscribed_project_ids`

**Severity:** Low - a malicious local request could submit `bot_token: "A".repeat(9 * 1024 * 1024)` — under the 10MB body limit but enough to fill `~/.termal/telegram-bot.json` with megabytes of junk.

`src/telegram_settings.rs:30-56` + `src/wire.rs:1012-1029`. Phase 1 single-user trust boundary makes this practically unexploitable, but the absence of any sanity check is worth noting.

**Current behavior:**
- No `MAX_BOT_TOKEN_LEN`, `MAX_PROJECT_ID_LEN`, or `MAX_SUBSCRIBED_PROJECTS` cap.
- A multi-MB JSON body is accepted up to the global 10MB limit.

**Proposal:**
- Add `MAX_BOT_TOKEN_LEN` (256 bytes), `MAX_PROJECT_ID_LEN` (256 bytes), `MAX_SUBSCRIBED_PROJECTS` cap.
- Reject in `update_telegram_config` with `ApiError::bad_request`.

## `SessionPaneView` `paneScrollPositions` in deps adds no reactivity

**Severity:** Low - the dependency on the dictionary identity is stable across renders for the same `pane.id`; mutations inside the dictionary do not trigger the effect. False reactivity impression for future readers.

`ui/src/SessionPaneView.tsx:1869-1900`. Either drop the dep with an `eslint-disable` comment explaining why, or capture the dependency narrowly (e.g., `paneScrollPositions[scrollStateKey]?.shouldStick`).

**Current behavior:**
- `paneScrollPositions` dict identity is stable across renders.
- Mutations inside the dict don't trigger the effect.
- The dep gives a false impression of reactivity.

**Proposal:**
- Drop the dep with an `eslint-disable` comment, or narrow to the specific value being read.

## `ConversationOverviewRail` per-segment fresh handlers and aria-label per render

**Severity:** Low - up to 160 segment buttons each get fresh `onClick`/`onKeyDown` arrow functions per render, plus a fresh `aria-label` string from `overviewSegmentLabel(segment, projection.items)` (an O(n) lookup against `projection.items.length`).

`ui/src/panels/ConversationOverviewRail.tsx:267-289`. Acceptable today, but as transcripts grow this is the next hot spot if rail rebuilds churn.

**Current behavior:**
- Each render creates 160 arrow functions and 160 aria-label strings.
- aria-label computation is O(n) against `projection.items`.

**Proposal:**
- Memoize per-segment handlers via a single delegated handler that reads the segment index from `data-conversation-overview-index`.
- Cache aria-labels alongside the segments.

## `ThemedCombobox` `useEffect` deps include `activeIndex`, tearing down listeners per keystroke

**Severity:** Low - the outside-pointer/keyboard handler effect re-attaches the global `pointerdown`/`keydown` listeners every time `activeIndex` changes (every ArrowUp/ArrowDown).

`ui/src/preferences-panels.tsx:1782-1859`. Functionally correct, but wasteful. If the same keystroke that triggered the change also fires a synthetic `keydown`, ordering between "old listener cleanup" and "new listener registration" is invisible to React.

**Current behavior:**
- Effect deps `[activeIndex, isOpen, onChange, options]` rebuild listeners per keystroke.
- Each open menu sees attach/detach churn.

**Proposal:**
- Move `activeIndex` into a ref synchronized with the state update; drop it from deps.
- Or split the effect into "attach listeners once when open" + "read activeIndex from a ref".

## `TelegramStatusResponse.lifecycle` is a stringly-typed enum

**Severity:** Low - backend hardcodes `"manual"` for Phase 0; frontend treats it as opaque (`status.lifecycle === "manual"` string compare). When Phase 1 introduces other values, a typo on either side fails silently.

`src/wire.rs:1036` + `ui/src/preferences-panels.tsx:1526`. All other lifecycle-style fields in the codebase (e.g., `SessionStatus`, `CodexThreadState`, `ParallelAgentStatus`) are typed enums with `#[serde(rename_all = "lowercase")]`.

**Current behavior:**
- `lifecycle: String` on the response.
- Frontend uses string comparison.
- No type safety against typos.

**Proposal:**
- Define a `TelegramLifecycle` enum (`Manual`, plus future variants behind feature flags or `#[serde(other)]`).
- Frontend gets `lifecycle: "manual" | "supervised" | ...` instead of `string`.
- Land before Phase 1 introduces new lifecycle values.

## `TelegramTestRequest.bot_token` semantic ambiguity for "use stored token"

**Severity:** Low - `Option<String>` with the implicit "fall back to saved token if absent" rule means there's no way for a caller to test ONLY a draft token without falling back. Asymmetric with `UpdateTelegramConfigRequest::bot_token: Option<Option<String>>`.

`src/wire.rs:1063-1066`. A wrapper that wants to validate a draft token before saving must either accept the fallback or check the response shape.

**Current behavior:**
- `bot_token: Option<String>`.
- Absent body falls back to saved token silently.
- No way to express "test ONLY this token; do not fall back".

**Proposal:**
- Add an explicit `useStoredToken: bool` flag.
- Or change the contract so `bot_token: null` means "use stored" and `bot_token: undefined` is rejected.
- Document the contract in the type comment.

## `spawn_reviewer_batch` mixed-server-instance error orphans spawned children with no caller-recoverable mapping

**Severity:** Medium - when a backend restart between parallel spawns triggers `mixedSpawnServerInstanceError`, the result still publishes `delegationIds`/`childSessionIds`/`spawned[]` arrays from spawns split across two backend instances, but `serverInstanceId: null` and `revision: null`. Wrapper callers cannot route follow-up `wait_delegations` or `cancel_delegation` calls correctly because revision/serverInstance metadata is null and the per-spawn server identity is lost in the per-spawn entries.

`ui/src/delegation-commands.ts:271-300`. The doc update at `docs/features/agent-delegation-sessions.md:236-239` only describes the outcome semantics, not the orphaning concern. A wrapper has no way to issue per-instance follow-ups because the per-spawn `SpawnDelegationCommandResult.serverInstanceId` lives inside `spawned[]` but the top-level `revision`/`serverInstanceId` are nulled.

**Current behavior:**
- Mixed-instance spawn batch produces `outcome: "error"` with `delegationIds`/`childSessionIds` populated.
- `revision: null`, `serverInstanceId: null` at the top level.
- Per-spawn entries inside `spawned[]` retain their original `serverInstanceId`, but wrapper callers must traverse the array to discover it.

**Proposal:**
- Either (a) include `serverInstanceId` per spawn entry inside the error packet so callers can route follow-ups, (b) auto-cancel the spawned children that came back from non-current instances, or (c) document the orphan-cleanup contract explicitly so wrapper authors know they cannot reliably drive these to completion through this surface.
- Add coverage for a wrapper caller that observes `error.kind === "mixed-server-instance"` and proves the documented recovery contract.

## `SpawnReviewerBatchCommandResult` shape diverges from `WaitDelegationsResult` discriminated union

**Severity:** Medium - the two wrapper-facing delegation result types use different shapes for the same outcome dimension; wrapper callers cannot symmetrically pattern-match `result.outcome === "error"` to find the failure detail.

`ui/src/delegation-commands.ts:135-144` defines `SpawnReviewerBatchCommandResult` as a flat object with `error: MixedServerInstanceErrorPacket | null` plus `failed: SpawnReviewerBatchFailure[]`. `outcome === "error"` can mean either mixed-server (`error` populated, `failed[]` may be empty) or all-items-fail (`error: null`, `failed[]` populated). The sibling `WaitDelegationsResult` type is a discriminated union: `outcome === "completed" | "timeout"` has `error?: never`, `outcome === "error"` has `error: WaitDelegationErrorPacket`. Wrapper callers must inspect both `error` and `failed[]` for spawn results, but only `error` for wait results.

**Current behavior:**
- `SpawnReviewerBatchCommandResult` is a flat object with `error` and `failed[]` independently populated.
- `WaitDelegationsResult` is a discriminated union with `error` exclusive to the `error` outcome.
- Wrapper callers cannot reuse one error-handling code path across both surfaces.

**Proposal:**
- Promote `SpawnReviewerBatchCommandResult` to a discriminated union mirroring `WaitDelegationsResult`, OR document the contract divergence with rationale so future MCP-wrapper authors do not assume parity.

## `SAFE_SPAWN_DELEGATION` allow-list omits deterministic `bad_request` and `not_found` strings the spawn route emits

**Severity:** Medium - the audit comment at `delegation-error-packets.ts:48-50` claims allow-listed messages match "deterministic backend bad_request strings the spawn route emits", but several deterministic shapes from `src/delegations.rs` are not present and collapse to `"Spawn delegation failed."`, removing useful structure for wrapper callers.

Missing entries cross-checked against `src/delegations.rs`: `parent session id is required` (line 205), `unknown project \`<id>\`` (line 296, format!-shaped), `delegation cwd \`<X>\` must stay inside project \`<name>\`` (line 304-307, format! with cwd path), and the canonical `session not found` from `local_session_missing` (line 266, 337). The cwd-shape regex pattern only matches the literal "Windows device namespace path" / "drive-relative Windows path" / "UNC path" wording; other cwd-validation strings (project-boundary, must-be-directory) collapse generically.

**Current behavior:**
- `parent session id is required` collapses to `"Spawn delegation failed."`.
- `unknown project \`<id>\`` collapses to `"Spawn delegation failed."` (status: 404 still preserved).
- `delegation cwd \`<X>\` must stay inside project \`<name>\`` collapses (carries cwd path; redaction is defensible but the project-boundary message itself is deterministic and useful to wrapper callers).
- `session not found` collapses (deterministic 404 shape that callers most plausibly need to discriminate beyond status alone).

**Proposal:**
- Audit `src/delegations.rs` `bad_request`/`not_found`/`conflict` constructors that the spawn route can emit and either add their literal/regex shapes to the allow-list or document why they collapse.
- Add per-pattern positive-and-negative unit tests so a regex/string drift cannot silently break the allow-list.
- Specifically: add `"session not found"` and `"parent session id is required"` to `SAFE_SPAWN_DELEGATION_MESSAGES`; consider a regex for the project-boundary cwd message.

## `mark_delegation_failed_after_start_error` leaks internal `dispatch_turn` error chains via `result.summary`

**Severity:** Medium - the round-52 spawn-failure-packet redactor closes the spawn return path, but the same disclosure shape still rides through `result.summary` populated when `start_delegation_child_turn` fails after a successful spawn record.

`src/delegations.rs:455-460` and `:1340-1349`. When `start_delegation_child_turn` returns `ApiError::internal(format!(...))` (any of the many `dispatch_turn` paths in `src/turn_dispatch.rs` carrying paths/persist/sled chains), the spawn route catches it, builds `format!("failed to start child session: {err.message}")`, and stashes that into `result.summary` via `mark_delegation_failed_locked`. `compact_delegation_public_summary` only line-trims and truncates — no path/error-chain redaction. The summary is forwarded verbatim through `getDelegationResultCommand` (`ui/src/delegation-commands.ts:639-655`) and `DelegationSummary.result.summary` exposed by `delegationSummary`. A wrapper can spawn successfully, then call `get_delegation_result` once the child fails to start, and read paths/error chains under `~/.termal/`.

**Current behavior:**
- `mark_delegation_failed_after_start_error` accepts the raw `err.message` without sanitization.
- `compact_delegation_public_summary` does not redact paths, persistence error chains, or sled/serde failure strings.
- Wrapper callers receive the raw chain through `getDelegationResultCommand` / `wait_delegations` / `DelegationSummary.result.summary`.

**Proposal:**
- Either (a) sanitize the `detail` before passing to `mark_delegation_failed_after_start_error` (drop the raw `err.message` in favour of a fixed "child session failed to start" plus a structured error code), or (b) introduce a parallel allow-list redactor on the wrapper side for `result.summary` / wait packets, with the same closed-set policy as the spawn-failure-packet redactor.
- Add coverage where `dispatch_turn` returns `ApiError::internal("...path under ~/.termal/...")` and assert the wrapper-visible `result.summary` is redacted to a generic shape.

## Vestigial `!message.contains("cursor-agent")` assertions no longer prove ordering after PATH-to-injection swap

**Severity:** Medium - round 52's swap of process-wide PATH mutation for `state.test_agent_setup_failures` injection left the original negative assertions in place. They are now trivially true and no longer pin the ordering claim.

`src/tests/delegations.rs:4716-4722, 4743-4747`. The original test (round 51 staged) used `ScopedEnvVar::set_path("PATH", &empty_path)` to force `validate_agent_session_setup` to fail with messages like `cursor-agent CLI not found on PATH`. The negative assertion `!title_err.message.contains("cursor-agent")` then proved metadata validation ran *before* readiness setup. Round 52 swapped the PATH approach for an injected setup failure with the message `"forced Cursor setup failure for ordering test"`. That message doesn't contain "cursor-agent" anyway, so the assertion is now trivially true even if metadata validation had been skipped. The ordering proof for the title and model paths now relies only on the implicit "title vs. injection" message diff.

**Current behavior:**
- `!title_err.message.contains("cursor-agent")` passes regardless of whether metadata validation actually ran first.
- `!model_err.message.contains("cursor-agent")` is the same trivial check.
- The new third assertion (`setup_err`) at lines 4747-4769 proves the injection itself fires, but the title and model halves no longer prove ordering against the injection sentinel.

**Proposal:**
- Replace `!title_err.message.contains("cursor-agent")` with `!title_err.message.contains("forced Cursor setup failure")` (or assert against the exact injection sentinel) so the negative assertion targets what actually fires when the gate runs.
- Same for the `model_err` assertion.

## `SpawnDelegationFailurePacket` lacks `kind` discriminator field carried by `WaitDelegationErrorPacket` variants

**Severity:** Low - wrapper callers handling both spawn failures and wait failures must use different strategies to identify the error class.

`ui/src/delegation-error-packets.ts:37-43`. The wait-error precedent (`status-fetch-failed`) has `kind: "status-fetch-failed"` plus `name`/`message`/`apiErrorKind`/`status`/`restartRequired`; the new spawn-failure packet has only the latter five fields. Wrapper code that wants to handle wait/spawn failures uniformly in one switch can use `name === "ApiRequestError"` as a proxy for "structured backend error" but cannot reuse a `kind`-based switch.

**Current behavior:**
- `WaitDelegationErrorPacket` variants discriminate by `kind`.
- `SpawnDelegationFailurePacket` (and `SpawnReviewerBatchFailure` extending it) has no discriminator.
- A future spawn-failure variant (e.g., "validation-rejected" pre-dispatch) requires retrofitting `kind` as a wire-compat change.

**Proposal:**
- Add `kind: "spawn-failed"` (or similar) to `SpawnDelegationFailurePacket` so it parallels `status-fetch-failed`, OR explicitly document in the data-model brief that spawn failures intentionally omit the `kind` discriminator (and why).

## `mark_delegation_failed_after_start_error` parallel surface: title-trim asymmetry between batch and single spawn

**Severity:** Low - `compactReviewerBatchRequest` (batch path) trims `title`; `compactCreateDelegationRequest` (single-spawn path) does not.

`ui/src/delegation-commands.ts:1004-1018`. Wrapper callers calling `spawn_delegation` directly send untrimmed titles to the wire, while `spawn_reviewer_batch` sends trimmed titles. Wrapper authors who later log/correlate the wire request body with the failure record will see different normalization rules for the two surfaces. The bugs.md proposal in round 51 explicitly noted the scope concern but only resolved it for the batch path.

**Current behavior:**
- Batch path trims `title` centrally before both wire and failure-record use.
- Single-spawn path does not trim; the backend's own trim happens server-side.
- Two parallel command surfaces normalize differently.

**Proposal:**
- Move title-trim into `compactCreateDelegationRequest` so both surfaces share normalization. The backend already trims so the wire-format change is harmless.
- Or document the divergence explicitly so wrapper authors do not assume parity.

## `compactReviewerBatchRequest` duplicates `compactCreateDelegationRequest` plus a title-trim shim

**Severity:** Low - two near-identical compact functions are an architecture-layer-boundary smell.

`ui/src/delegation-commands.ts:1004-1018`. The intention is clear (only-batch trim), but the helper inlines reconstruction of the request after stripping `title`. A future change to `compactCreateDelegationRequest` (e.g., trimming `model`, normalizing `cwd`) will not propagate through the batch path automatically.

**Current behavior:**
- `compactReviewerBatchRequest` reads `compactCreateDelegationRequest`'s output, then re-applies a title-only trim on top.
- The duplicated structure means future `compactCreateDelegationRequest` changes need to be re-checked against the batch path.

**Proposal:**
- Either fold trim into `compactCreateDelegationRequest` (preferred — see entry above) or wrap with a clear `withTrimmedTitle(compacted)` helper at the same module level so the layering is visible.

## `shouldOpenConversationMarkerContextMenu` narrowed to `.message-meta` without rationale comment

**Severity:** Low - round 52 narrowed marker-menu trigger from "anywhere on the assistant message shell" to "header only", but the only signal in the code is the constant `CONVERSATION_MARKER_CONTEXT_MENU_HEADER_SELECTOR` and the test that was rewritten to match. A future reader will not see the rationale and may revert it.

`ui/src/panels/conversation-markers.tsx:198-218`. Round-51 allowed right-click anywhere on the assistant shell (not on a code/link/etc.) to open the marker menu. Round-52 narrows this to only header-area right-clicks, preserving native plain-text context menu in the body. The behavior is a deliberate trade-off but no header comment documents it.

**Current behavior:**
- `shouldOpenConversationMarkerContextMenu` requires `target.closest(".message-meta")` AND `root.contains(header)`.
- The constant explains the selector but not the trade-off.
- A future change reverting to the broader selector would not break the existing tests in an obvious way (tests would need to be updated alongside).

**Proposal:**
- Add a header comment to `shouldOpenConversationMarkerContextMenu` documenting why the menu only opens on the message-meta region (preserves native context menu on plain assistant body text; user opens marker menu intentionally via the timestamp/author header).

## `normalizeReviewerBatchRequests` synchronous throw bypasses the spawn-failure-packet redactor

**Severity:** Low - argument-validation errors propagate out of `spawn_reviewer_batch` as plain JS exceptions, bypassing `reviewerBatchFailure`/`spawnDelegationFailurePacket` entirely.

`ui/src/delegation-commands.ts:957-983`. `validateDelegationMetadataText` `RangeError` (title too long), `TypeError` (malformed item), `RangeError` (prompt empty), and `normalizeTransportId` errors at the parent-session-id boundary all surface as raw thrown exceptions outside the result envelope. The exception messages themselves are deterministic and don't carry paths/secrets, so this is a contract/UX issue rather than an information-disclosure leak. But it complicates the audit story for "every wrapper-facing failure is a redacted packet."

**Current behavior:**
- Most failures arrive as redacted `SpawnReviewerBatchFailure` packets in `result.failed[]`.
- A subset surfaces as raw thrown exceptions outside the result envelope.
- Contract is inconsistent across the same wrapper-facing surface.

**Proposal:**
- Either (a) catch synchronous validation errors inside `spawnReviewerBatchWithTransport` and emit them as per-item failure packets (with index attribution and a fixed "invalid request" message), or (b) document the synchronous throw boundary explicitly in `delegation-error-packets.ts` so future maintainers don't assume the redactor is the only failure surface.

## Marker context-menu clamping does not re-clamp on viewport resize

**Severity:** Low - the `useLayoutEffect` clamping the menu position depends only on `[contextMenu]`. A viewport resize while the menu is open leaves it at the original clamped position; a sufficiently aggressive resize could leave the menu off-screen.

`ui/src/panels/conversation-markers.tsx:309-327`. Native browser context menus typically close on resize anyway, but this menu does not — it stays open through scroll (intentional, per existing bug entry) and now persists across resizes too (unintentional).

**Current behavior:**
- Menu clamp recomputes only when `contextMenu` state changes (open/move).
- Window resize during open menu leaves the position unchanged.
- A small window after a large initial open can leave the menu partially off-screen.

**Proposal:**
- Listen for `resize` events on the window while the menu is open and re-run `clampConversationMarkerContextMenuPosition`.
- Or close the menu on resize (matching the pre-existing scroll-close trade-off but in reverse: scroll is intentional, resize is rare).

## Conversation overview controller activation effect deps drop `messageCount`

**Severity:** Low - pure message-growth above the rail-render threshold without resize/scroll won't refresh the layout snapshot.

`ui/src/panels/conversation-overview-controller.ts:277`. The rail-activation effect deps were trimmed from `[isActive, messageCount, refreshLayoutSnapshot, sessionId, shouldRender]` to `[isActive, refreshLayoutSnapshot, sessionId, shouldRender]`. Since `shouldRender = messageCount >= MIN_MESSAGES`, only the threshold-crossing transition triggers a re-run. If `messageCount` grows substantially while staying above threshold (50 → 500 over a long session), this effect won't re-run. A separate effect at line 279 captures resize/scroll-driven snapshot refreshes, so most cases are covered, but a pure message-count growth without layout change wouldn't refresh the snapshot from this path.

This overlaps with the existing "Conversation overview rail snapshots can go stale after transcript growth" entry but is a narrower, more recent symptom: the effect's deps trimming makes it impossible to recover via this path even if a future fix added `messageCount` back.

**Current behavior:**
- Activation effect deps no longer include `messageCount`.
- Threshold-crossing transition triggers re-run.
- Pure growth above threshold without resize/scroll does not.

**Proposal:**
- Either add `messageCount` back to deps (cheap re-run when count changes), or document why the upstream layout-refresh path is sufficient.

## Marker-menu clamp fallback path (`rect.width === 0` → `offsetWidth`) is untested

**Severity:** Low - the JSDOM-realistic case where `getBoundingClientRect` returns zeros never exercises the fallback path; production browsers exercise only the rect path.

`ui/src/panels/conversation-markers.tsx:537-570`. `clampConversationMarkerContextMenuPosition` reads `menu.getBoundingClientRect()` and falls back to `menu.offsetWidth/offsetHeight` if width/height are 0. The existing clamp test at `AgentSessionPanel.test.tsx:719-748` mocks `getBoundingClientRect` to return `width: 180`, exercising only the rect path. The fallback (the JSDOM-default case) is never exercised. If a regression broke the fallback (e.g., dropped the `||` guard), the production-realistic path could clamp incorrectly while tests still pass.

**Current behavior:**
- One clamp test mocks `getBoundingClientRect` to non-zero.
- The fallback path that runs in JSDOM by default is unexercised.
- Asymmetric coverage between the two clamp branches.

**Proposal:**
- Either remove the rectSpy and rely on `Object.defineProperty(HTMLElement.prototype, "offsetWidth", ...)` to seed the fallback, or add a second clamp test that specifically pins the offset-fallback path.

## Assistant message-shell with `tabIndex={-1}` lacks `role`/`aria-label`

**Severity:** Low - restoring focus to an unlabeled focusable container provides poor screen-reader feedback.

`ui/src/panels/AgentSessionPanel.tsx:832`. `tabIndex={canOpenMarkerMenu ? -1 : undefined}` makes the assistant message-shell programmatically focusable so `trigger.focus()` works after Escape. But the shell `<div>` has no `role` or `aria-label` describing what is focused. Screen readers will announce a generic group/region without context, which is a regression in clarity from the previous implementation that didn't restore focus to the shell.

**Current behavior:**
- Shell is programmatically focusable for marker-menu Escape restore.
- No `role` or `aria-label` describes the focusable element.
- Screen readers announce generic content on focus restore.

**Proposal:**
- Add `aria-label` (e.g., `"Assistant message {message.id}"`) or use `role="group"` with a title.
- Or restore focus to the assistant message header / `.message-meta` instead of the shell so the focused region carries existing semantics.

## `AgentSessionPanel.tsx` exceeds 2000-line architecture rubric threshold

**Severity:** Low - file remains over the documented TSX file-size budget after round-51/52 hook extractions.

`ui/src/panels/AgentSessionPanel.tsx`. The marker-context-menu hook extraction shrunk this file but it remains over budget. The CLAUDE.md instruction explicitly asks to "Keep new modules small and focused — the project has a few very large files already and we are actively splitting them smaller, not larger." Round 52 added 17 lines to make the `visibleMessageIds` memo explicit but did not trim other surface area.

**Current behavior:**
- File is 2175 lines after round 52.
- Architecture rubric §9 sets ~2000-line scrutiny threshold for TSX components.
- Candidates remain (e.g., `renderMarkedMessageCard` factory, `SessionConversationPage` memo body).

**Proposal:**
- Track for a future split round; candidates are the `renderMarkedMessageCard` callback factory and the `SessionConversationPage` memo body, both of which could become their own modules.

## `spawn_reviewer_batch` `partial` outcome documentation under-specifies same-instance constraint

**Severity:** Low - the doc prose describes `partial` as "at least one spawn succeeded and at least one item failed", but the implementation also requires that all successful responses share one backend instance for the outcome to be `partial`.

`docs/features/agent-delegation-sessions.md:236-239`. If failures + cross-instance successes coincide, the implementation collapses to `outcome: "error"` with `MixedServerInstanceErrorPacket` regardless of `failed.length`. The doc's prose lists the cross-instance restart case only on the `error` line, obscuring that any cross-instance success during a partial-failure batch also triggers the error path.

**Current behavior:**
- Doc prose says `partial` = "at least one spawn succeeded AND at least one item failed".
- Implementation requires all successful responses share one backend instance for `partial`.
- Wrapper callers reading the brief may write code expecting any non-empty `failed[]` with non-empty `spawned[]` yields `partial`.

**Proposal:**
- Tighten doc prose to "`partial` means at least one spawn succeeded, at least one item failed, AND every successful response shares one backend instance"; "`error` covers every-item-failed AND any-cross-instance-success cases (the latter additionally sets `error.kind === "mixed-server-instance"` and nulls revision metadata)".

## `useConversationMarkerContextMenu` focus-restore rAF is not cancelled on hook unmount

**Severity:** Low - the rAF for `trigger.focus()` after Escape may fire after unmount, calling focus on a possibly-detached trigger.

`ui/src/panels/conversation-markers.tsx:229-238 closeContextMenu`. When called with `restoreFocus: true`, the helper schedules `window.requestAnimationFrame(() => trigger.focus())` but never cancels it when the hook unmounts. If the parent panel unmounts (session destroyed, pane closed) between Escape and the next animation frame, the rAF still fires. Today benign — focus on a detached element is a no-op — but if the trigger element is later re-attached in a different context (rare with virtualization), focus could land somewhere unexpected.

**Current behavior:**
- `restoreFocus: true` schedules an unguarded rAF for `trigger.focus()`.
- Hook unmount does not cancel that rAF.
- Trigger may be detached or re-attached when the rAF fires.

**Proposal:**
- Track an in-flight focus-restore rAF id at hook scope and cancel it from a `useEffect` cleanup on hook unmount.
- Or guard with a "still mounted" flag set to false in the cleanup.

## Marker menu Escape fires both the document-level and menu-div Escape handlers

**Severity:** Low - `event.preventDefault()` does not stop native propagation to a `document.addEventListener` listener; both `closeContextMenu` calls run on a single Escape press.

`ui/src/panels/conversation-markers.tsx:301-306` (menu div onKeyDown) and the document-level `keydown` listener registered at the same hook. When Escape is pressed inside the menu, both handlers fire; `setContextMenu(null)` runs twice (second is a no-op) and two rAF focus-restore frames are queued.

**Current behavior:**
- Escape in the menu fires the menu div's `onKeyDown` AND the document `keydown` listener.
- Both call `closeContextMenu({ restoreFocus: true })`.
- Two rAF focus-restore frames queued (harmless, but wasted).

**Proposal:**
- Use native `event.stopPropagation()` (not React's `preventDefault`) inside the menu div's keyDown handler when handling Escape.
- Or remove the document-level Escape handler when focus is inside the menu.

## `visibleMessageIds` Set is built twice in `AgentSessionPanel.tsx`

**Severity:** Low - duplicated work on every visible-messages change; the round-51 marker hook input could reuse the existing inline `Set`.

`ui/src/panels/AgentSessionPanel.tsx:687-690` (new memoized `visibleMessageIds` for the marker context-menu hook input) and `:659-661` (inline `Set` construction inside `visiblePendingPrompts` useMemo). The new memo can be reused inside `visiblePendingPrompts` to avoid the duplicate `Set` allocation.

**Current behavior:**
- Two separate `Set` allocations from the same `visibleMessages.map(m => m.id)`.

**Proposal:**
- Take a dependency on the memoized `visibleMessageIds` from `visiblePendingPrompts` instead of constructing a fresh `Set`.

## Marker menu keyboard nav `Math.max(0, findIndex)` skips item 0 when no menu item has focus

**Severity:** Low - corner case where the user lands on a non-menuitem inside the menu (e.g., the separator `<div>`); ArrowDown would skip item 0.

`ui/src/panels/conversation-markers.tsx:461-464 handleConversationMarkerContextMenuKeyDown`. `Math.max(0, menuItems.findIndex(item => item === document.activeElement))` collapses `findIndex === -1` to `currentIndex = 0`. ArrowDown then computes `nextIndex = 1`, skipping item 0. In normal flow focus is moved to item 0 via the focus-rAF before arrow keys are pressed, so this rarely surfaces — but if focus reaches the separator `<div>` or any non-menuitem inside the menu, the first ArrowDown press skips item 0.

**Current behavior:**
- `findIndex === -1` → currentIndex = 0.
- ArrowDown → nextIndex = 1 (skips item 0).
- ArrowUp → nextIndex = N-1 (last item, skipping item 0).

**Proposal:**
- Treat `-1` as "before the menu" for ArrowDown (focus item 0) and "after" for ArrowUp (focus item N-1).
- Or distinguish "no current menu-item focus" from "focus on item 0" with a separate sentinel.

## Marker action menu has no keyboard-reachable trigger

**Severity:** Low - marker removal actions are exposed through a custom menu that keyboard users cannot discover or open.

`ui/src/panels/AgentSessionPanel.tsx:829-832` attaches the marker action menu to the assistant message shell's `contextmenu` handler, while the shell uses `tabIndex={-1}`. The menu itself now supports focus movement and Escape, but opening it still depends on a pointer/right-click path unless the browser sends a context-menu event from a focused shell.

**Current behavior:**
- Assistant message shells are not reachable through normal tab navigation.
- The marker action menu opens from the shell context-menu pointer path.
- Marker deletion is not discoverable as a keyboard action.

**Proposal:**
- Add a focusable toolbar/menu trigger with `aria-haspopup`, or make a tabbable trigger support ContextMenu/Shift+F10/Enter with deterministic positioning.
- Keep native context-menu preservation for links, code blocks, images, and selected text.

## `useConversationMarkerContextMenu` removed scroll-close behavior — fixed positioning means visible drift on scroll

**Severity:** Low - the marker menu renders as a fixed body portal, so user scroll or resize can leave actions visibly detached from the message they target.

`ui/src/panels/conversation-markers.tsx:350` renders the menu through a `document.body` portal using viewport coordinates captured at open. The hook no longer closes or repositions the menu on scroll/resize, so the transcript can move while the menu remains at its old viewport position.

**Current behavior:**
- Menu is positioned by fixed viewport coordinates at open time.
- Page scroll or resize can move the target message without moving the menu.
- The portal can remain visible while pointing at stale message context.

**Proposal:**
- Restore scroll/resize dismissal with cleanup, or actively recompute/clamp the menu against the current trigger.
- Add regression coverage for the chosen behavior so streaming/programmatic scroll concerns remain explicit.

## Marker menu behavior depends on the `.message-meta` styling class

**Severity:** Low - a styling refactor can silently remove or move the behavioral hit target for marker actions.

`ui/src/panels/conversation-markers.tsx:51` makes `.message-meta` the selector for opening the marker context menu. That ties the interaction contract to a CSS class that otherwise reads as presentation-only, so a future message-card restyle can break marker actions without touching the hook or tests.

**Current behavior:**
- The hook opens only when the context-menu event originates from `.message-meta`.
- Message-card styling controls whether the behavior is reachable.
- There is no explicit data attribute or trigger component documenting the behavioral contract.

**Proposal:**
- Use an explicit data attribute such as `data-conversation-marker-menu-trigger`, or expose a dedicated trigger slot.
- Keep a focused contract test so presentation-only class changes cannot remove marker actions.

## `spawn_delegation` still exposes raw create-delegation errors

**Severity:** Medium - single-spawn wrapper callers can still receive backend 500 details while batch spawns now return sanitized failure packets.

`ui/src/delegation-commands.ts:221` awaits `transport.createDelegation` and lets failures reject directly. `spawn_reviewer_batch` now converts equivalent failures through `spawnDelegationFailurePacket`, but `spawn_delegation` is also a documented command surface and has no equivalent sanitized result contract.

**Current behavior:**
- `spawn_reviewer_batch` sanitizes spawn failures before returning structured output.
- `spawn_delegation` still propagates raw `createDelegation` failures.
- Wrapper/MCP-facing callers can observe internal diagnostics on the single-spawn path.

**Proposal:**
- Give `spawn_delegation` the same sanitized failure/result contract as the batch path.
- Or require the wrapper boundary to route thrown `ApiRequestError` values through `spawnDelegationFailurePacket` before exposing them.

## Mixed-instance delegation packet wording is operation-specific

**Severity:** Low - spawn-batch callers can receive an error message that describes a delegation status batch instead of a spawn operation.

`ui/src/delegation-error-packets.ts:163` builds the mixed-server-instance packet used by delegation command code. The packet is now reused by spawn-batch handling, but its wording still points at a "delegation status batch", which makes wrapper-facing errors harder to interpret.

**Current behavior:**
- The mixed-instance helper is shared by status and spawn command paths.
- The user-facing message names only the status-batch operation.
- Spawn-batch callers can receive a technically correct but misleading diagnostic.

**Proposal:**
- Make the packet wording operation-neutral.
- Or accept an operation label so status and spawn callers get accurate diagnostics.

## `scheduleConversationOverviewRailBuild` module-level FIFO queue is shared across all controller instances

**Severity:** Note - a slow rail build in pane A delays pane B by one frame; cleanup story across module reloads / HMR is subtle.

`ui/src/panels/conversation-overview-controller.ts:33-87`. The module-level `pendingConversationOverviewRailBuildTasks: ConversationOverviewRailBuildTask[]`, `conversationOverviewRailBuildFrameId: number | null`, and `nextConversationOverviewRailBuildTaskId: number = 1` are shared across all controllers/sessions/panes. Acceptable per the rAF cadence (60Hz = 16ms/frame). The cleanup logic (cancel-on-empty-queue, splice-by-task-id) is correct but the global-state coupling means HMR / module reloads have subtle behavior.

**Current behavior:**
- All rail builds across all panes serialized through one global FIFO queue.
- One slow task delays all subsequent panes by one frame.
- Module-level globals make cleanup-across-HMR subtle.

**Proposal:**
- Defer (no concrete bug today). Consider in a future round whether per-pane queues would simplify reasoning, especially as multi-pane scenarios become more common.

## `forward_telegram_text_to_project` mutates forwarding state before prompt acceptance

**Severity:** Medium - a failed Telegram prompt submission can still mutate assistant-forwarding cursor state for a prompt the backend never accepted.

`src/telegram.rs:854-857`. `arm_assistant_forwarding_for_telegram_prompt` mutates `state.forward_next_assistant_message_session_id` and may re-baseline `last_forwarded_assistant_message_id` / `_text_chars` before `termal.send_session_message(...)?` succeeds. If the local POST rejects or fails, the handler returns `Err(...)` after mutating state. Because the poll loop has already advanced `next_update_id`, the final persist can save forwarding state for a prompt that never reached the backend; if a later call fails after the POST but before the digest refresh, the relay can also resend the same Telegram prompt on the next poll.

**Current behavior:**
- `arm_*` mutates forwarding state before `send_session_message` succeeds.
- The prompt-send failure path does not roll that state back.
- The outer poll loop may persist the mutated forwarding state because `next_update_id` advanced.
- A later digest-fetch failure after a successful prompt POST can still replay the Telegram prompt on the next poll.

**Proposal:**
- Compute the pre-prompt cursor without mutating, then apply forwarding-state changes only after `send_session_message` succeeds.
- Or snapshot and roll back forwarding state on prompt-send failure.
- Add send-failure and post-send digest-failure regressions.

## Telegram assistant forwarding can capture a pre-existing active turn

**Severity:** Medium - a Telegram-originated prompt can cause the relay to later forward assistant text from an already-active desktop/local turn.

`src/telegram.rs:854` arms assistant forwarding by fetching the current session and recording the latest assistant text cursor before the Telegram prompt is accepted. If that cursor points at a partially streaming assistant message from an existing local turn, later growth of that message can look like a truncated Telegram-forwarded message and be sent to the linked Telegram chat even though it predates the Telegram prompt.

**Current behavior:**
- `arm_assistant_forwarding_for_telegram_prompt` baselines the latest assistant text from the current primary session.
- It can run while the session is already active for a non-Telegram turn.
- Later growth of that pre-existing assistant text can be treated as reply text to forward.

**Proposal:**
- Only baseline settled assistant text, or store an explicit pre-prompt boundary that skips the currently active turn without enabling truncated-resend forwarding.
- Add coverage where a desktop/local turn is already streaming before a Telegram prompt is forwarded.

## Telegram assistant forwarding drains only the digest primary session

**Severity:** Medium - forwarding is armed for a specific session, but polling only checks whichever session is currently primary in the project digest.

`src/telegram.rs:854` records `forward_next_assistant_message_session_id`, but `sync_telegram_digest` forwards assistant text only for `digest.primary_session_id` at `src/telegram.rs:914`. If the digest primary moves before the Telegram-originated turn settles, the armed session may never be drained. The cursor is also global, so polling another primary can overwrite the baseline and cause the original reply to be skipped.

**Current behavior:**
- Telegram prompt forwarding arms one session id.
- Regular digest sync only polls the digest primary session.
- Assistant forwarding cursor state is global rather than per session.

**Proposal:**
- Track assistant-forwarding cursors per session.
- Or have `sync_telegram_digest` process `forward_next_assistant_message_session_id` in addition to the digest primary before any cross-session baseline update.

## Telegram long-message forwarding loses chunk-level progress on failure

**Severity:** Medium - a failure after one chunk of a long assistant message causes the next retry to resend already-delivered chunks.

`src/telegram.rs:1151-1160` sends all chunks for a message before recording `last_forwarded_assistant_message_id` and `last_forwarded_assistant_message_text_chars`. If chunk 2 fails after chunk 1 was sent, the cursor is not advanced to reflect the partial chunk success. The next poll retries the same message from the beginning, duplicating chunk 1 in Telegram.

**Current behavior:**
- Progress is recorded per assistant message, after all chunks for that message are sent.
- A mid-message chunk failure loses the already-sent chunk position.
- Retrying the same long message resends earlier chunks.

**Proposal:**
- Track chunk-level progress for in-flight long messages, or restructure forwarding so successful chunks can be resumed without replaying already-sent chunks.
- Add emoji/long-message retry coverage that fails a later chunk and proves the earlier chunk is not duplicated.

## Telegram chunking counts Rust chars instead of Telegram UTF-16 units

**Severity:** Medium - emoji-heavy messages can exceed Telegram's sendMessage limit even when they pass the relay's chunk-size check.

`src/telegram.rs:1221` documents that Telegram rejects bodies over 4096 UTF-16 code units, but `chunk_telegram_message_text` enforces `TELEGRAM_MESSAGE_CHUNK_CHARS` using Rust `char` count. Surrogate-pair-heavy text can stay under 3500 Unicode scalar values while exceeding Telegram's UTF-16-unit limit, causing send failures and repeated retries.

**Current behavior:**
- Chunk size is measured with `text.chars()`.
- The comment claims this keeps multi-byte text under Telegram's UTF-16-unit limit.
- Emoji-heavy content can still produce chunks above Telegram's actual limit.

**Proposal:**
- Chunk by UTF-16 code units while preserving valid character boundaries.
- Add tests for emoji/surrogate-pair-heavy messages.

## CSS context-menu pattern duplicated between pane-tab and conversation-marker variants

**Severity:** Low - two near-third "context menu" features now share ~80% of the same CSS shell; the third copy will be the trigger for extraction but it should be promoted to a `.context-menu` family before then.

`ui/src/styles.css:3981-4023` (new `.conversation-marker-context-menu*`) and `:2506-2546` (existing `.pane-tab-context-menu*`). Same `position: fixed`, z-index ordering, `color-mix(in srgb, var(--surface-white) ...)` background pattern, `box-shadow: 0 20px 40px color-mix(in srgb, var(--ink) 14%, transparent)`, hover/focus blue mix, `*-item-danger` red. Differences are only `min-width`, `border-radius` (custom 1rem vs `var(--control-radius)`), padding values, and `border: 1px solid var(--line)` vs unbordered. The pattern is reusable as a `.context-menu` / `.context-menu-item` / `.context-menu-item-danger` family.

**Current behavior:**
- Two near-duplicate context-menu CSS blocks.
- Small variations are unique-to-call-site.
- Future third instance would copy a third near-duplicate.

**Proposal:**
- Promote the shared shell + item rules into a base `.context-menu` set.
- Let `.pane-tab-context-menu` and `.conversation-marker-context-menu` carry only their unique tweaks (`min-width`, `border-radius`, `border`, separator).
- Defer if the variations are deliberately divergent — but mark this as a known cluster so the third instance triggers extraction.

## `SessionPaneView.tsx` near-bottom early-out is captured at `isSending` flip, not reactive

**Severity:** Low - the catchup branch never schedules for the started-near-bottom-then-scrolled-away case, contrary to what the comment promises.

`ui/src/SessionPaneView.tsx:2024`. The effect has dependency array `[isSending, pane.viewMode, scrollStateKey]`, and `isMessageStackNearBottom()` reads from `messageStackRef.current` (not reactive). The effect's near-bottom decision is therefore evaluated only at the moment `isSending` flips true. If the user starts the send near bottom but scrolls away while the request is in flight, the catchup branch (`scheduleSettledScrollToBottom` via `followLatestMessageForPromptSend`) never schedules.

**Current behavior:**
- Near-bottom snapshot captured only at `isSending` true→false transition.
- User scrolling away during the in-flight request bypasses catchup.
- Comment overstates the guarantee ("schedule the settled-poll catchup here to bring the user's prompt into view once it lands").

**Proposal:**
- Add a reactive signal (e.g., a derived `nearBottomAtSendStart` captured into the effect's deps via a ref-based subscription).
- Or update the comment to match the actual behavior ("when the user is near bottom AT THE TIME isSending toggled, defer entirely to the post-message-land effect").

## Marker context-menu close-on-document-pointerdown re-registers listeners per state value change

**Severity:** Low - rapid right-click switches between messages cause attach/detach churn.

`ui/src/panels/AgentSessionPanel.tsx:751-757`. The effect re-registers listeners every time `markerContextMenu` changes (since the dep is the entire state value, not just a boolean). For a single open/close cycle this is fine; but rapid right-click switches between messages cause attach/detach churn.

**Current behavior:**
- Effect dep is the full marker-context-menu state object.
- Each open/move/close event re-registers `pointerdown`/`keydown`/`scroll` listeners.

**Proposal:**
- Gate the dep on `markerContextMenu === null` so listeners attach once when the menu opens and detach once when it closes.

## `src/tests/telegram.rs` header documents fewer pinned axes than the file currently covers

**Severity:** Note - test ownership header is stale relative to the +285-line additions.

`src/tests/telegram.rs:1-12` describes test ownership as "pin two pieces of that adapter: `parse_telegram_command` ... and `render_telegram_digest` / `build_telegram_digest_keyboard`". The +285-line additions now cover assistant-forwarding partial-progress, no-baseline forwarding, unknown char-count re-forwarding, unknown-session-status gating, error classification, log sanitization, and prompt byte-limit — these go beyond what the header claims.

**Current behavior:**
- Header at lines 1-12 covers two pinned axes.
- File now pins many additional axes.

**Proposal:**
- Update the header to enumerate the additional pinned axes (or summarize as "Telegram relay test surface: command parsing, digest rendering, assistant forwarding, error classification, log sanitization, prompt limits").
- Or split the file if the assistant-forwarding family becomes its own pinned axis.

## Delayed overview readiness remounts the conversation subtree

**Severity:** Medium - the long-session transcript can lose scroll, virtualizer measurements, text selection, and message-card local state when the rail flips from pending to ready.

`ui/src/panels/AgentSessionPanel.tsx:950` renders bare `conversationContent` until `conversationOverview.shouldRenderRail` becomes true, then wraps that same subtree in `.conversation-with-overview`. React treats the changed parent structure as a different tree, so `ConversationMessageList` and its message cards can remount at the readiness boundary.

**Current behavior:**
- Long sessions initially render the conversation without the overview wrapper.
- After delayed rail activation, the same conversation subtree is reparented under `.conversation-with-overview`.
- The readiness flip can discard transcript DOM, virtualizer refs, measured layout, text selection, and per-card local state.

**Proposal:**
- Keep the wrapper stable whenever `conversationOverview.shouldRender` is true.
- Gate only the rail node, readiness styling, or a placeholder inside the stable wrapper.
- Add DOM-identity and scroll-position coverage proving the message list does not remount when the rail becomes ready.

## Conversation overview rail snapshots can go stale after transcript growth

**Severity:** Medium - once the delayed overview rail is ready, new messages can leave overview layout and viewport state based on the old transcript until a scroll or resize occurs.

`ui/src/panels/conversation-overview-controller.ts:277` delays the initial rail activation, but the activation effect no longer depends on `messageCount` and the steady-state effect refreshes layout on initial ready, scroll, or resize rather than transcript growth. Appending messages to an already-long transcript can therefore leave overview proportions, marker placement, and viewport projection stale during live streaming.

**Current behavior:**
- Initial overview rail activation waits for the transcript paint.
- After the rail is ready, layout refreshes are event-driven by scroll/resize.
- Message growth alone does not refresh layout snapshots.

**Proposal:**
- Keep the initial double-RAF delay, but add a separate ready-state refresh keyed on transcript growth that calls `refreshLayoutSnapshot()` without hiding the rail.
- Add coverage for appending or streaming messages into an already rail-ready long transcript.

## `messageCreatedDeltaIsNoOp` lacks semantic-change negative coverage

**Severity:** Medium - the identical-replay tests do not prove the no-op predicate still material-applies when the message payload changes while metadata stays equal.

`ui/src/live-updates.ts:320` compares the existing message payload and metadata to decide whether a `messageCreated` replay is no-op. Current tests cover identical duplicate replay behavior, but do not keep id/index/preview/count/stamp the same while changing a semantic message payload field. An over-broad predicate could drop a real `messageCreated` update while the current tests still pass.

**Current behavior:**
- Duplicate identical `messageCreated` replay coverage exists.
- Semantic-change negative cases are missing for same id/index/metadata.
- Same-id pending prompt cleanup interaction is not directly pinned.

**Proposal:**
- Add negative cases in `live-updates.test.ts` for same id/index/metadata with changed message payload.
- Add coverage for same-id pending prompt cleanup so no-op detection cannot skip required prompt removal.

## Near-bottom prompt-send early return lacks direct scroll coverage

**Severity:** Medium - the prompt-send stutter fix is not directly pinned by a near-bottom pending-POST test.

`ui/src/SessionPaneView.tsx:2024` returns early when the message stack is already near bottom so the old-bottom smooth scroll does not race the later post-message scroll. Existing scroll coverage pins the far-from-bottom catch-up path, but not this near-bottom skip. A regression could reintroduce the old-target smooth scroll and visible stutter without failing the current suite.

**Current behavior:**
- Near-bottom sends skip the old-bottom smooth-scroll effect.
- Far-from-bottom prompt catch-up is covered.
- No test starts near bottom, keeps the POST pending, grows `scrollHeight`, and asserts no old-target smooth scroll fires before the prompt lands.

**Proposal:**
- Add an `App.scroll-behavior.test.tsx` case that starts near bottom, sends with a pending POST, grows `scrollHeight`, and asserts no old-target smooth scroll occurs before the prompt lands.

## Deferred overview rail activation lacks cancellation coverage

**Severity:** Low - the double-requestAnimationFrame activation guard has stale-session and cancellation logic that is not directly tested.

`ui/src/panels/conversation-overview-controller.ts:189` queues two animation frames before activating the overview rail. The effect cancels queued frames and checks the expected session id, but the tests cover only the happy path where the rail eventually appears. Switching sessions or dropping below the overview threshold before queued frames drain could regress without test failure.

**Current behavior:**
- Rail activation is delayed by two animation frames.
- Cleanup cancels queued frame ids and guards against stale session ids.
- No test drains mocked frames after a session switch or threshold drop to prove no stale rail appears.

**Proposal:**
- Add a mocked-RAF test that switches sessions or drops below the overview threshold before queued frames drain, then asserts no stale overview rail appears.

## Returning to bottom leaves stale virtualized scroll-kind classification

**Severity:** Low - cancelling idle compaction on bottom re-entry leaves `lastUserScrollKindRef.current` unchanged.

`ui/src/panels/VirtualizedConversationMessageList.tsx:2263` clears the idle compaction timer after a native downward scroll returns near bottom. That timer is normally the path that expires the cached wheel/touch/key scroll-kind classification. If it is cancelled without clearing `lastUserScrollKindRef`, a later native scroll with no input prelude, such as scrollbar-thumb movement, can reuse stale `"incremental"` / `"seek"` classification.

**Current behavior:**
- Returning to bottom clears the idle compaction timer.
- The cached last user scroll kind is not cleared at the same boundary.
- Later native scrolls without a preceding input event can inherit stale classification.

**Proposal:**
- Preserve the cooldown timestamp, but clear or separately expire `lastUserScrollKindRef` when cancelling idle compaction on bottom re-entry.

## Telegram command suffix parsing ignores the actual bot username

**Severity:** Low - commands addressed to another bot can be treated as TermAl commands in a linked group chat.

`src/telegram.rs:1487` strips anything after `@` in the command name without checking whether the suffix matches the TermAl bot username. In a shared or group chat, a delivered `/stop@other_bot` style command is parsed as `/stop` and can trigger TermAl behavior.

**Current behavior:**
- `parse_telegram_command` uses `raw_name.split('@').next()`.
- Any bot suffix is ignored.
- Commands intended for another bot can match TermAl command names.

**Proposal:**
- Resolve/cache this bot's username via `getMe` and reject non-matching suffixes.
- Or restrict the relay to private chats by default.

## Telegram startup output still advertises first-touch `/start` linking

**Severity:** Low - runtime setup instructions conflict with the new trusted-chat binding requirement.

`src/telegram.rs:44` still prints `chat: not linked; send /start to the bot to link it` when no chat binding exists. The README and handler behavior now require `TERMAL_TELEGRAM_CHAT_ID` or an existing trusted `telegram-bot.json` binding for fresh relays, so the startup output points operators at a flow that no longer establishes trust.

**Current behavior:**
- Fresh relays no longer bind the first chat that sends `/start`.
- Startup output still says `/start` is the linking path.
- Operators can follow the printed instruction and remain unlinked.

**Proposal:**
- Update the startup message to point at `TERMAL_TELEGRAM_CHAT_ID` or an existing trusted state-file binding.

## Telegram JSON parsing paths lack sample-shape coverage

**Severity:** Low - error envelope and session-status serde behavior can regress while current tests that construct internal structs still pass.

`src/telegram.rs:567` and related Telegram/TermAl response parsing now include behavior for Telegram error envelopes and `TelegramSessionStatus` values. The tests mostly construct internal Rust structs directly, so sample JSON with `error_code`, unknown status, or missing status can drift from the actual API shapes without being caught.

**Current behavior:**
- Classifier/status tests mostly use constructed Rust values.
- Telegram error-envelope JSON parsing is not pinned with sample payloads.
- TermAl session status parsing for unknown/missing status is not pinned with sample payloads.

**Proposal:**
- Add sample JSON deserialization tests for Telegram error envelopes.
- Add TermAl session status sample JSON tests for known, unknown, and missing status values.

## Telegram-forwarded text has no per-chat rate cap

**Severity:** Medium - any linked chat can still fan out prompt submissions quickly enough to create a burst of local backend and agent work.

`src/telegram.rs:775-787` now rejects Telegram prompts above `MAX_DELEGATION_PROMPT_BYTES = 64 * 1024` before calling `forward_telegram_text_to_project`, but accepted prompts are still not rate-limited per chat. A linked chat can submit many below-limit prompts or action commands in quick succession, each becoming local backend work and possibly an agent turn.

**Current behavior:**
- Oversized Telegram prompts are rejected by UTF-8 byte length.
- Below-limit prompts and action commands are forwarded unchanged.
- No per-minute or burst cap exists per linked chat before backend work starts.

**Proposal:**
- Add a per-minute / per-chat prompt and action-command rate cap so a linked chat cannot fan out N HTTP calls per second.

## Telegram relay forwards full assistant text to Telegram by default

**Severity:** Medium - assistant replies can include code, local file paths, file contents, or secrets and are sent to a third-party service without an explicit opt-in.

`src/telegram.rs:1151-1160`. The relay chunks and forwards the full settled assistant message body to Telegram once the session is no longer active. This goes beyond the compact project digest and sends arbitrary model output off-machine by default.

**Current behavior:**
- The Telegram digest path is compact, but settled assistant messages are forwarded in full.
- Assistant text may contain local workspace details or user-provided secrets.
- Users enabling the relay do not get a separate opt-in for full-content forwarding.

**Proposal:**
- Make full assistant text forwarding an explicit opt-in setting.
- Keep digest-only forwarding as the default for Telegram integrations.
- Document the third-party content exposure and add any practical redaction/truncation before full forwarding.

## Telegram `getUpdates` batch processing is unbounded and re-runs on poll-iteration panic

**Severity:** Medium - a Telegram update burst (real attack, retry storm, or accidental flood) becomes a multiplicative wave of HTTP calls into the local backend, and a panic mid-batch re-runs the same updates on the next poll.

`src/telegram.rs:44-78` and `src/telegram.rs:304`. The relay accepts the entire `Vec<TelegramUpdate>` Telegram returns and walks each update through `handle_telegram_update`, which can issue multiple outbound HTTP calls per update (digest fetch, send_message, action dispatch, session fetch). State is persisted once at the end of the iteration; a panic mid-batch leaves `next_update_id` un-advanced and Telegram resends the same batch on the next poll, amplifying the effect.

**Current behavior:**
- `getUpdates` does not pass an explicit `limit`, so Telegram returns up to its server-side default (100).
- A 100-update batch can fan out to several hundred backend HTTP calls.
- Mid-batch panic loses all per-update state (including advanced `next_update_id`), and Telegram replays.

**Proposal:**
- Cap `getUpdates` `limit` (e.g., 25) on the request side.
- Persist `next_update_id` per update inside the batch loop rather than once at the end.
- Add a per-iteration backoff after errors so a sustained failure does not tight-loop.

## `preferStreamingPlainTextRender` prop name and "Three render paths" comment are stale after the unified-render refactor

**Severity:** Medium - public-shaped prop semantics drift silently from a removed implementation; future readers see a name describing code that no longer exists.

`ui/src/message-cards.tsx:311, 327, 359-388`. The `StreamingAssistantTextShell` render path was removed; the prop now functionally means "this is the live-streaming assistant message". The leading comment block ("Three possible render paths...") describes the old three-path implementation, contradicted by the next paragraph saying "always renders through `<DeferredMarkdownContent>`". Test names and the prop signature carry the old name.

**Current behavior:**
- `preferStreamingPlainTextRender` is the only render-path discriminator but its name suggests a removed plain-text shell.
- The leading comment describes three paths; only one survives.

**Proposal:**
- Rename to `isStreamingAssistantTextMessage` (matching the local `isStreamingAssistantMessage` variable already used inside `MessageCardImpl`).
- Replace the "Three possible render paths" sentence with the current single-path explanation.
- Pure refactor, no behavior change.

## CSS bubble `width: fit-content` transition causes horizontal layout reflow at turn end

**Severity:** Medium - the "stable component subtree across stream → settle" goal is partially undermined by a CSS-driven layout jump.

`ui/src/styles.css:4448-4452`. `:has(.markdown-table-scroll)` applies `width: fit-content; max-width: min(96rem, 96%)` to the bubble. With the new `deferAllBlocks: true` policy a streaming bubble has NO `.markdown-table-scroll` until the turn settles (the table sits in `.markdown-streaming-fragment` instead). When streaming ends, the bubble's effective `max-width` jumps from default `42rem` to `96rem` AND its `width` switches to `fit-content` — the bubble grows wider, producing a visible horizontal reflow at the same moment the React subtree was supposed to be stable.

**Current behavior:**
- During streaming the bubble follows the prose-default sizing (`42rem` cap).
- On settle the `:has(.markdown-table-scroll)` selector engages and the bubble jumps to `fit-content` / 96rem cap.

**Proposal:**
- Anticipate `width: fit-content` while streaming if a `|`-line has been seen (e.g., a class on the streaming-fragment placeholder that triggers the same selector).
- Or: document the layout shift as accepted and add a regression that asserts bubble width remains stable across the stream→settle transition so any future drift is visible in tests.

## Mermaid aspect-ratio sizing can clip constrained diagrams

**Severity:** Medium - constrained Mermaid iframes can hide diagram content instead of only removing blank frame space.

`ui/src/mermaid-render.ts:134-152` sizes Mermaid iframes with `aspectRatio: ${frameWidth} / ${frameHeight}` + `height: auto`. The change fixes the wide-blank-frame regression, but the iframe document still keeps the SVG at intrinsic width while hiding vertical overflow. When `max-width: 100%` constrains the iframe, the frame height can shrink without the inner SVG scaling with it, so wide or tall diagrams can lose bottom content instead of simply removing unused whitespace.

**Current behavior:**
- The outer iframe height is driven by CSS `aspect-ratio` and constrained width.
- The inner Mermaid SVG can remain at intrinsic dimensions.
- The iframe hides vertical overflow, so constrained diagrams can be clipped at the bottom.

**Proposal:**
- Keep explicit intrinsic height for horizontally-scrollable diagrams, or make the inner SVG scale with the iframe width before using aspect-ratio sizing.
- Add visual/regression coverage for constrained wide and tall diagrams.

## Asymmetric `orchestrator_auto_dispatch_blocked` between two persist-failure rollback sites

**Severity:** Medium - an Error session can remain auto-dispatch-eligible after a runtime-exit commit failure while disk and memory disagree.

`src/session_lifecycle.rs:449` defensively sets `record.orchestrator_auto_dispatch_blocked = true` on persist-failure rollback in the stop-session path. `src/turn_lifecycle.rs:455` does NOT mirror that defensive set in the runtime-exit rollback path, and the inner block at `turn_lifecycle.rs:413` has already explicitly cleared the flag to `false` before the failed `commit_locked`. Net effect: if the runtime-exit commit fails, the session in-memory state is `SessionStatus::Error` with the "Turn failed: …" message, but the orchestrator can still observe it as eligible for auto-dispatch.

**Current behavior:**
- `stop_session` rollback sets `orchestrator_auto_dispatch_blocked = true` defensively.
- `handle_runtime_exit_if_matches` rollback leaves the flag at whatever the inner block last wrote (`false`).
- An Error session with a failed persist commit can still be re-dispatched.
- The new tests do not pin `orchestrator_auto_dispatch_blocked` in either rollback path.

**Proposal:**
- Either mirror the `session_lifecycle.rs` defensive set (`true`) in `turn_lifecycle.rs`, or document why the asymmetry is intentional.
- Tighten the persist-failure tests to also pin `orchestrator_auto_dispatch_blocked`, `runtime`, `runtime_stop_in_progress`, and the "stopped/failed" message presence.

## Mermaid fallback loader lives in the message card renderer

**Severity:** Low - Mermaid fallback loading and cache ownership are mixed into an already large rendering component.

`ui/src/message-cards.tsx:182` now owns dynamic import failure classification and global fallback script loading, while `mermaid-render.ts` already owns Mermaid rendering configuration and queueing.

**Current behavior:**
- Mermaid fallback loader/cache logic lives in `message-cards.tsx`.
- Rendering, fallback loading, and message-card composition are coupled in the same large module.

**Proposal:**
- Move fallback loading into `mermaid-render.ts` or a small `mermaid-loader.ts`.
- Keep message cards calling a single render helper.

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

## Duplicate remote delta hydrations fall through to unloaded-transcript delta application

**Severity:** Medium - duplicate in-flight hydration callers receive `Ok(false)`, which every delta handler treats as "no repair happened; continue applying the delta".

The in-flight map suppresses duplicate `/api/sessions/{id}` fetches, but it does not coordinate the waiting delta handlers. For a summary-only remote proxy, a concurrent text delta or replacement can still run against missing messages and trigger a broad `/api/state` resync; a message-created delta can partially mutate an unloaded transcript before the first full hydration finishes.

**Current behavior:**
- The first delta for an unloaded remote session starts full-session hydration.
- A duplicate delta for the same remote/session sees the in-flight key and returns `Ok(false)`.
- Callers continue into the narrow delta path as if no hydration was needed.

**Proposal:**
- Return a distinct outcome such as `HydrationInFlight`, or have duplicates wait/queue behind the first hydration.
- After the first hydration completes, re-check the session transcript watermark before applying queued or retried deltas.
- Add burst/concurrent same-session delta coverage proving only one remote fetch occurs and duplicate deltas do not mutate unloaded transcripts.

## Text-repair hydration lacks live rendering regression coverage

**Severity:** Medium - the lower-revision text-repair adoption path is covered only by a classifier unit test.

The new adoption rule is intended to fix the user-visible bug where the latest assistant message stays hidden until an unrelated focus, scroll, or prompt rerender. The current coverage proves the pure classifier returns `adopted`, but it does not prove the live hook requests the flagged hydration, adopts the lower-revision session response after an unrelated newer live revision, flushes the session slice, and renders the repaired text immediately.

**Current behavior:**
- `classifyFetchedSessionAdoption` has a unit test for divergent text repair after a newer revision.
- No hook or app-level regression drives `/api/sessions/{id}` through the live-state path and asserts immediate transcript rendering.

**Proposal:**
- Add a `useAppLiveState` or `App.live-state.reconnect` regression where text-repair hydration is requested, a newer unrelated live event advances `latestStateRevisionRef`, the session response resolves at the original request revision, and the active transcript updates without any extra user action.

## Timer-driven reconnect fallback can stop after `/api/state` progress before SSE proves recovery

**Severity:** Medium - a fallback snapshot can refresh visible UI while the live EventSource transport is still unhealthy.

`ui/src/app-live-state.ts:2068` disables `rearmUntilLiveEventOnSuccess` when a same-instance `/api/state` response makes forward revision progress, unless the recovery path is the manual-retry variant. A successful `/api/state` fetch proves that polling can reach the backend and can repair visible state, but it does not prove the SSE stream has reopened or can deliver later assistant deltas. If the transport remains broken, a later live message can stay hidden until another reconnect/error/user action restarts recovery.

**Current behavior:**
- Timer-driven reconnect fallback asks to keep polling until live-event proof.
- Same-instance `/api/state` forward progress disables that live-proof rearm path for non-manual recovery.
- UI state can look refreshed while the EventSource transport is still unconfirmed.

**Proposal:**
- Split "snapshot refreshed UI" from "transport recovered" in the reconnect state machine.
- Keep reconnect polling armed until `confirmReconnectRecoveryFromLiveEvent()` runs from a data-bearing SSE event, unless a cause-specific recovery path intentionally documents a different contract.
- Add a regression that adopts same-instance `/api/state` progress through the timer-driven reconnect path, keeps SSE unopened/unconfirmed, advances timers, and asserts another fallback poll is scheduled.

## Remote hydration in-flight cleanup can race with the RAII guard

**Severity:** Low - clearing `remote_delta_hydrations_in_flight` by key can remove or later invalidate a newer in-flight hydration for the same remote/session.

The remote hydration guard removes its `(remote_id, session_id)` key on drop. `clear_remote_applied_revision` can also remove keys for a remote while an older hydration guard is still alive. If a later hydration inserts the same key after that cleanup, the older guard can drop afterward and remove the newer marker, allowing duplicate hydrations despite the guard.

**Current behavior:**
- In-flight hydration entries are keyed only by `(remote_id, session_id)`.
- Remote continuity cleanup can remove a live key while the guard that owns it is still alive.
- A stale guard drop cannot distinguish its own entry from a newer entry with the same key.

**Proposal:**
- Store a unique token or generation per in-flight entry and remove only when the token still matches.
- Or avoid clearing live in-flight markers during remote continuity reset; let the owning guard retire its own marker.
- Add cleanup tests covering overlapping guards and per-remote cleanup.

## Lagged force-adopt marker clearing on EventSource reconnect lacks coverage

**Severity:** Low - the frontend now clears an armed lagged recovery marker on EventSource error/reconnect, but no test pins that boundary.

The new baseline guard covers same-stream stale recovery after a newer delta, but a separate hazard is an old `lagged` marker surviving across a closed EventSource into a new stream. The implementation clears the marker on reconnect/error cleanup, yet no regression proves a stale lower/same-instance state on the new stream cannot be force-adopted.

**Current behavior:**
- `clearForceAdoptNextStateEvent` runs during EventSource error/reconnect cleanup.
- Existing lagged tests do not cross an EventSource boundary.

**Proposal:**
- Add a reconnect test that dispatches `lagged`, triggers `error`, opens a new EventSource, and sends a lower/same-instance state that must not be force-adopted.

## Remote hydration dedupe coverage bypasses the production burst path

**Severity:** Low - the current duplicate-hydration test manually seeds the in-flight map instead of driving real bursty remote deltas.

The test pins the duplicate branch, but it would not catch a regression where the first real hydration leaks the guard, where a successful hydration does not clear the marker, or where multiple actual same-session delta handlers still issue duplicate remote session fetches.

**Current behavior:**
- The test inserts an in-flight key directly.
- It does not prove the first production hydration inserts and clears the guard.
- It does not prove bursty same-session deltas issue only one remote session fetch.

**Proposal:**
- Add coverage for a successful hydration path that asserts the guard is removed afterward.
- Add a burst/concurrent same-session delta case that asserts only one remote session fetch is issued.

## `apply_remote_state_if_newer_locked` `force: bool` parameter is unnamed at call sites

**Severity:** Low - seven call sites pass `false` and one passes `true`; readers cannot tell what `force` means without consulting the function signature.

`apply_remote_state_if_newer_locked` was extended with a `force: bool` parameter so that `apply_remote_lagged_recovery_state_snapshot` can bypass the same-revision replay gate. The parameter is correct, but the convention scales poorly: a future caller that copies a neighbouring `false` from any of the seven existing sites will inherit the gated behaviour without realising the parameter exists, and a future maintainer who needs the bypass at a different site will have to re-derive what the boolean means.

**Current behavior:**
- `apply_remote_state_if_newer_locked(&mut inner, remote_id, &remote_state, None, false)` appears at seven call sites.
- One new call site passes `true` for lagged-recovery force-apply.
- The doc-comment on the function explains the parameter, but the call sites do not self-document.

**Proposal:**
- Replace `force: bool` with a typed `enum SnapshotApplyMode { GateBySnapshotRevision, ForceApplyAfterLagged }` (or similar). All existing call sites become `SnapshotApplyMode::GateBySnapshotRevision`; the lagged-recovery site reads `SnapshotApplyMode::ForceApplyAfterLagged` and self-documents.
- Optional: also push the bypass-gate into a tiny inline comment at the lagged-recovery site naming the upstream invariant (`api_sse.rs::state_events` yields `state` immediately after `lagged` within one `tokio::select!` arm).

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

## Per-session hydration burst has no cooldown beyond in-flight deduplication

**Severity:** Low - `ui/src/app-live-state.ts:2329, 2421, 2437`. The new `startSessionHydration(delta.sessionId)` calls trigger `GET /api/sessions/{id}` (full transcript fetch) on every problematic delta. `hydratingSessionIdsRef` deduplicates concurrent fetches per session, but it does not rate-limit successive fetches: once a hydration completes, the next problematic delta on the same session immediately schedules another full transcript fetch. On a flaky network with bursty deltas, a hydration→delta→hydration loop is possible, each iteration shipping the entire transcript over the wire.

**Current behavior:**
- In-flight dedup via `hydratingSessionIdsRef` collapses simultaneous calls to one round-trip.
- After completion, the next problematic delta immediately schedules another fetch with no cooldown.
- Phase-1 local-only deployment makes this practically free; future remote-host or flaky-network use exposes the storm risk.

**Proposal:**
- Add a per-session cooldown timestamp ("don't re-hydrate the same session within Nms of the last completed hydration unless the new delta carries a revision strictly greater than the one that started the previous hydration").
- Or document the burst as intentional given the local-only deployment cost; add a comment naming the trade-off so future reviewers don't keep flagging it.

## Watchdog-inversion tests don't assert the "Waiting for the next chunk of output…" affordance state

**Severity:** Low - `ui/src/App.live-state.deltas.test.tsx:3439` and `ui/src/App.live-state.watchdog.test.tsx:625`. The two recent inverted tests assert that the recovered text becomes visible, but say nothing about the "Waiting for the next chunk of output…" affordance. After the recovery snapshot adopts (the deltas test's snapshot has `status: "idle"`, the watchdog test's stays `status: "active"`), the affordance state is the most user-visible signal of whether recovery actually replaced the wedged UI vs just rendered the recovered text somewhere on the page.

**Proposal:**
- In `deltas.test.tsx`: add `expect(screen.queryByText("Waiting for the next chunk of output...")).not.toBeInTheDocument();` after the assertion that the recovered chunk is visible (recovery snapshot is idle, affordance should disappear).
- In `watchdog.test.tsx`: add an assertion clarifying expected affordance state for the still-active recovery (the assistant chunk now sits at the boundary, so the affordance should NOT be present).

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

## Implementation Tasks

- [ ] P2: Add Telegram settings API/security regressions:
  cover nullable `botToken`/`defaultProjectId`/`defaultSessionId` clearing, token-file restrictive permissions on Unix, generic client-facing file errors without absolute paths, reduced token-mask suffix exposure, malformed JSON returning the project `ApiError` envelope, `/api/telegram/test` preserving Telegram upstream error text, explicit saved-token opt-in plus cooldown, and empty `subscribedProjectIds` matching the TypeScript contract.
- [ ] P2: Add Telegram settings file concurrency regressions:
  simulate UI config save racing relay state persistence, plus malformed/unreadable existing settings during `persist_telegram_bot_state`, and assert token/config plus `chatId`/`nextUpdateId` are not lost.
- [ ] P2: Add Telegram preferences panel RTL coverage:
  cover initial status load, save payload normalization, clearing default project/session values with `null`, saved-token test flow, API error display, stale default-session clearing, default-project auto-subscription, and the AppDialogs Telegram tab path.
- [ ] P2: Add delegation command error-contract regressions:
  cover `spawn_delegation` and `spawn_reviewer_batch` backend-unavailable/restart diagnostics, wrong-parent/session redaction, safe spawn-message allowlist patterns, and operation-appropriate mixed-instance packet wording.
- [ ] P2: Add Telegram assistant-forwarding ownership/order regressions:
  cover an already-active non-Telegram turn before a Telegram prompt, prompt POST failure after forwarding state is prepared, post-send digest failure, and a digest primary-session switch before the armed Telegram reply settles.
- [ ] P2: Add Telegram long-message chunk retry and UTF-16 chunking coverage:
  fail a later chunk after an earlier chunk is sent and assert retry does not duplicate it; add emoji/surrogate-pair-heavy text proving chunks respect Telegram's UTF-16 code-unit limit.
- [ ] P2: Add marker context-menu interaction and cleanup coverage:
  cover a keyboard-reachable marker action trigger, focus/Escape propagation, `.message-meta` trigger-contract behavior, portal cleanup when pane/session activity changes, and scroll/resize dismissal or explicit retained-position behavior.
- [ ] P2: Add overview rail growth, remount, and cancellation coverage:
  after the delayed rail is ready, append messages and assert layout/viewport snapshots refresh; switch sessions or drop below threshold before queued animation frames drain and assert no stale rail appears; assert the conversation list DOM and scroll position survive rail readiness.
- [ ] P2: Add `messageCreatedDeltaIsNoOp` semantic-change negatives:
  keep id/index/preview/count/stamp equal while changing message payload and assert a material apply; include same-id pending prompt cleanup coverage.
- [ ] P2: Add near-bottom prompt-send early-return scroll coverage:
  start near bottom, send with a pending POST, grow `scrollHeight`, and assert no old-target smooth scroll fires before the prompt lands.
- [ ] P2: Add virtualized bottom re-entry scroll-kind expiry coverage:
  return to bottom, cancel idle compaction, then issue a native scroll without wheel/touch/key prelude and assert stale `lastUserScrollKindRef` classification cannot leak.
- [ ] P2: Add Telegram command suffix and startup-message coverage:
  reject `/command@other_bot` once a bot username is known or private-chat-only mode is chosen, and assert the no-chat startup message points to `TERMAL_TELEGRAM_CHAT_ID` / trusted state binding rather than first-touch `/start`.
- [ ] P2: Add Telegram JSON sample deserialization coverage:
  parse Telegram error envelopes with `error_code`, and TermAl session responses with known, unknown, and missing statuses.
- [ ] P2: Add reconnect-specific gapped session-delta recovery coverage:
  arm reconnect fallback polling, reopen SSE, dispatch an advancing stamped `textDelta`/`textReplace` across a revision gap, and assert live text renders before snapshot repair while recovery remains pending until authoritative repair succeeds.
- [ ] P2: Add equal-revision gap repair snapshot adoption coverage:
  skip a non-session revision, optimistically apply a later session delta, then return `/api/state` at the same revision and assert the skipped global state is adopted instead of rejected as stale.
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
- [ ] P2: Add non-send action restart live-stream delta-on-recreated-stream coverage:
  the round-13 fix proves `forceSseReconnect()` is called on cross-instance `adoptActionState` recovery, but does not dispatch live deltas through the recreated EventSource. Submit an approval/input-style action after backend restart, then dispatch assistant deltas on the new `EventSourceMock` and assert they render in the active transcript bubble.
- [ ] P2: Add live text-repair hydration rendering regression:
  drive the live-state hook or app through text-repair hydration after an unrelated newer live revision and assert the active transcript renders the repaired assistant text without scroll, focus, or another prompt.
- [ ] P2: Add AgentSessionPanel deferred-tail component regressions:
  cover switching from a non-empty deferred transcript to an empty current session, and same-id updated assistant text through the rendered component path (`useDeferredValue`, pending-prompt filtering, and the virtualized list), not only the exported helper.
- [ ] P2: Add lagged-marker EventSource reconnect-boundary regression:
  dispatch `lagged`, trigger EventSource error/reconnect, then send a lower/same-instance state on the new stream and assert the old marker cannot force-adopt it.
- [ ] P2: Add remote hydration dedupe production-path coverage:
  drive bursty same-session remote deltas through the production hydration path, assert only one remote session fetch is issued, and assert the in-flight guard is cleared after successful hydration.
- [ ] P2: Add failed manual retry reconnect-rearm regression:
  cover manual retry hitting a transient failure, then the next scheduled attempt adopting a newer same-instance snapshot while polling still continues until SSE confirms.
- [ ] P2: Add timer-driven reconnect same-instance-progress live-proof regression:
  trigger the non-manual reconnect fallback path, adopt a same-instance `/api/state` snapshot with forward progress while SSE remains unopened/unconfirmed, advance timers, and assert fallback polling continues until a data-bearing live event confirms recovery.
- [ ] P2 watchdog wake-gap stop-after-progress regression:
  trigger watchdog wake-gap recovery, adopt same-instance `/api/state` progress, and assert no additional reconnect polling occurs before a later live event.
- [ ] P2: Cover the index clamp-on-shrink branch in `MarkdownDiffView` and `RenderedDiffView`:
  re-render the parent with a smaller `regions`/`segments` array while `currentChangeIndex`/`currentRegionIndex` points past the new end and assert the counter snaps to "Change/Region 1 of N" while prev/next still wrap correctly. Today the existing prev/next tests only exercise wrap-around at full length; the `current >= changeCount/regionCount` clamp branch in the `useEffect` is unexercised.
- [ ] P2: Add rendered diff render-budget coverage:
  create many Mermaid/math rendered regions and assert the preview applies the same document-level caps as a single `MarkdownContent` document.
- [ ] P2: Add single-target rendered diff navigation coverage:
  assert prev/next scrolls the only Markdown diff change and the only rendered diff region even though the selected index does not change.
- [ ] P2: Route the new lagged-recovery reconnect test through the textDelta fast-path it documents:
  the new `App.live-state.reconnect.test.tsx` test exercises the revision-gap branch (the `messageCreated` delta omits `sessionMutationStamp` so it falls into the resync fallback). Add `sessionMutationStamp` so the delta routes through the matched-stamp fast-path that the surrounding `handleDeltaEvent` comment is most concerned about, OR rename the test to clarify it covers the revision-gap branch specifically and add a sibling test for the textDelta fast-path.
- [ ] P2: Split the bad-live-event + workspaceFilesChanged test into isolated arrange-act-assert phases:
  `ui/src/backend-connection.test.tsx:1225-1261` co-fires the stale `delta` and the `workspaceFilesChanged` event in one `act()`. The assertion `countStateFetches() === hydratedStateFetchCount` is satisfied if either side skips confirmation, so the test cannot pinpoint which side regressed. Dispatch `workspaceFilesChanged` alone first and assert no fetch fired; then add the stale delta separately and re-assert.
- [ ] P2: Add frontend stop/failure delta-before-snapshot terminal-message coverage:
  dispatch cancellation/update deltas before the same-revision snapshot and assert appended stop/failure terminal messages remain rendered without relying on a later unrelated refresh.
- [ ] P2: Replace `function scrollIntoView() { ... = this; }` capture pattern in `AgentSessionPanel.test.tsx`:
  `AgentSessionPanel.test.tsx:1515, 1568, 1680` rely on `this`-binding inside a method-form function. Future swc/esbuild config that arrow-rewrites methods or any "use strict" tightening would silently break the capture. Use `vi.spyOn(HTMLElement.prototype, 'scrollIntoView').mockImplementation(function (this: HTMLElement) { scrolledNode = this; })` and rely on `mockRestore()` to clean up.
- [ ] P2: Tighten `expectRequestErrorDeferredUpdatesOnly` to assert deferred-update payloads:
  `app-session-actions.test.ts:158-172` checks shape (every call is a function, never null) but never invokes the deferred functions. A regression where the deferred function returns a stale or `null` next state would still pass. Invoke each captured updater with a fixed `prev` and assert the resulting next state.
- [ ] P2: Add doc-comment to `clear_active_turn_file_change_tracking` enumerating callers and intent:
  the helper is now shared across normal-completion sites (`src/session_interaction.rs`, `src/state_accessors.rs`, etc.) and the two new persist-failure rollback sites (`src/session_lifecycle.rs:450`, `src/turn_lifecycle.rs:455`). A future "preserve grace deadline" tweak motivated by one purpose would silently change the other. Document the unconditional-wipe contract so readers see both intents.
- [ ] P2: Add Rust persist-failure rollback negative-coverage tests:
  `src/tests/session_stop.rs:560`, `src/tests/session_stop_runtime.rs:897` only cover the failure path. Add a sibling test that runs the same setup with succeeding persistence and asserts the post-stop record retains its expected (non-cleared) fields, proving the cleanup is gated on the failure branch and that the helper is not unconditionally clearing on every commit.
- [ ] P1: Add Telegram-relay unit tests for the pure helpers introduced in `src/telegram.rs`:
  cover `chunk_telegram_message_text` (empty, exact-3500-char, under-limit, no-newline-in-window hard-split, newline-in-window soft-split, multi-byte / emoji char-vs-UTF16-unit, trailing-newline preservation), `telegram_turn_settled_footer` for `idle` / `approval` / `error` / unknown-status arms, `telegram_error_is_message_not_modified` against the Telegram error wording, and a serde-decode round-trip for `TelegramUpdate` / `TelegramChatMessage` against a real-shape `getUpdates` JSON snapshot to pin the snake_case contract.
- [ ] P1: Add `forward_new_assistant_message_if_any` logic-level coverage:
  refactor the message-walking branch into a pure helper that takes a `Vec<TelegramSessionFetchMessage>` + state and returns a forwarding plan (or use a fake `TelegramApiClient` / `TermalApiClient`). Cover the active-status gate, the cold-start baseline policy, a Telegram-originated first reply that must be forwarded, the streaming-then-settled re-forward via char-count growth, and per-message progress recording on mid-batch send failure.
- [ ] P1: Add direct splitter tests for `splitStreamingMarkdownForRendering(text, { deferAllBlocks: true })`:
  `ui/src/markdown-streaming-split.test.ts` currently has zero direct coverage for `deferAllBlocks`. Cover closed pipe-table followed by prose, closed fence followed by prose, closed math followed by prose, multiple closed blocks (cut at the earliest), nested constructs, and identity behavior on inputs containing no block constructs.
- [ ] P1: Pin the "no remount across streaming → settled transition" claim with DOM-identity assertions:
  `ui/src/MarkdownContent.test.tsx:1456` and `ui/src/MessageCard.test.tsx:245-285` assert the rendered shape but do not assert that React preserved the same DOM nodes across the rerender. Capture a stable child DOM node (e.g., a paragraph from the settled prefix) before flipping `isStreaming` to false, then assert `container.contains(savedNode)` and reference equality after the rerender.
- [ ] P2: Restore discrimination in `expectRenderedMarkdownTableContains`:
  `ui/src/App.live-state.deltas.test.tsx:296-315` was relaxed to accept either `.markdown-table-scroll table` or `.markdown-streaming-fragment` after `deferAllBlocks: true` shipped. The relaxation silently passes if a streaming table never settles. Split into two helpers (`expectStreamingTableFragmentContains` for active-streaming phase, `expectSettledTableContains` for after-turn-end) called at the right phases, or advance the test through whatever signal flips `isStreaming` to false before the assertion.
- [ ] P2: Move the Mermaid bundle-URL assertion out of the `appendChild` spy:
  `ui/src/MarkdownContent.mermaid-fallback.test.tsx:54` asserts inside the mock implementation; failure traces point at the spy rather than the test body. The post-render assertions at lines 77-78 (`expect(appendedScripts).toHaveLength(1); expect(appendedScripts[0]?.src).toBe(expectedBundleSrc);`) already pin the contract; delete the inline `expect()`.
- [ ] P2: Pin the heavy-content gate is bypassed during streaming:
  `ui/src/MessageCard.test.tsx:245-284` confirms shape but does not assert `.deferred-markdown-placeholder` is absent during streaming. Add an assertion for an `isStreaming` assistant message regardless of size, and pair with a long-enough streaming message (over the heavy threshold) confirming the gate stays bypassed.
- [ ] P2: Add a wire-projection round-trip test for `TelegramSessionFetchMessage`:
  the parallel narrow projection of `Message` in `src/telegram.rs:481-515` will silently desync if `wire_messages.rs` renames the discriminator or the `Text` variant fields — serde will deserialize into `Other` and the relay will go silent on text messages. Round-trip a representative `Message::Text` payload through `TelegramSessionFetchMessage` and assert the `Text` arm matched.
- [ ] P2: Add app-level watchdog coverage for replayable appliedNoOp deltas:
  `ui/src/live-updates.test.ts` now pins reducer-level `appliedNoOp` for `messageCreated`, `textReplace`, `messageUpdated`, `commandUpdate`, `parallelAgentsUpdate`, and marker replays, and `App.live-state.deltas.test.tsx` covers `textReplace` plus duplicate `messageCreated`. Add siblings for the remaining no-op replay types and assert the watchdog still fires within `LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS + 3000` ms.
- [ ] P2: Add genuine-divergence reconciliation coverage for same-revision unknown-session deltas:
  `docs/architecture.md` now documents that session creation advances the main revision, and `ui/src/app-live-state.ts` cross-links that contract. Add a coverage test that (a) sets the client up with a session list missing `session-X`, (b) dispatches a same-revision session delta for `session-X` (where `latestStateRevisionRef.current === delta.revision`) — asserting NO immediate `/api/state` fetch, then (c) dispatches the next authoritative `state` event including `session-X` and asserts it adopts cleanly.
- [ ] P2: Tighten delegation-ordering vestigial assertions:
  replace `!message.contains("cursor-agent")` with `!message.contains("forced Cursor setup failure")` (or assert the exact unique metadata-validation strings) at `src/tests/delegations.rs:4716-4722, 4743-4747` so the ordering proof targets the active injection sentinel rather than a string that happens to be absent regardless.
- [ ] P2: Add `spawnDelegationFailurePacket` non-`ApiRequestError` branch coverage:
  throw `new TypeError("network down: file://internal/path")` from `createDelegation`; assert `failure.name === "TypeError"`, `failure.message === "Spawn delegation failed."`, `apiErrorKind: null`, `status: null`, `restartRequired: null`. Round 51's partial-failure test threw a plain `Error`, but round 52 upgraded that throw to `ApiRequestError`, removing the only assertion exercising the non-ApiRequestError branch.
- [ ] P2: Add per-pattern coverage for the spawn-failure allow-list:
  `delegation-error-packets.ts:64-72` defines seven safe regex patterns (prompt-bytes, title-chars, model-chars, parent-active-limit, nesting depth, cwd Windows shapes, request-failed-status) plus six exact-string entries. Only the parent-active-limit pattern is currently exercised. Add `it.each(...)` covering each safe message/pattern with positive (passes) and at least one negative near-miss (collapses to GENERIC) per entry.
- [ ] P2: Add `restartRequired: true` propagation through the spawn-failure packet:
  construct an `ApiRequestError("backend-unavailable", ..., { status: 503, restartRequired: true })` and assert `failed[0].restartRequired === true`. Today's tests only build `ApiRequestError` with the default `restartRequired: false`, so a regression that drops the field at the packet boundary would not be caught.
- [ ] P2: Cover `normalizeReviewerBatchRequests` non-array/null/non-object item rejection:
  the normalizer throws `TypeError("spawn_reviewer_batch requests must be an array")` on non-array input and `TypeError("reviewer request N must be an object")` on null/primitive items. Neither is currently tested. Extend the existing rejection test with `null` input, `"not-an-array"` input, and `[null]` / `[42]` items.
- [ ] P2: Make the mixed-server-instance sort assertion load-bearing:
  the existing batch test happens to feed `"server-a"`, `"server-b"` (alphabetical = insertion order). Use ids whose insertion order differs from sorted order (e.g., `"server-z"` then `"server-a"`) so a refactor that drops `[...new Set(...)].sort()` would fail.
- [ ] P2: Cover `useConversationMarkerContextMenu` `isActive` close path:
  open the marker menu in an active panel, then re-render with a different `activeSessionId` (or rerender the parent so `isActive` flips false), and `await waitFor(() => expect(screen.queryByRole("menu", ...)).not.toBeInTheDocument())`. The hook's effect at `conversation-markers.tsx:283-287` is currently uncovered.
- [ ] P2: Reset module-level rail-build FIFO state between overview controller tests:
  `pendingConversationOverviewRailBuildTasks`, `nextConversationOverviewRailBuildTaskId`, and `conversationOverviewRailBuildFrameId` persist across Vitest workers. Today only one test exists in `conversation-overview-controller.test.tsx`, but the test is built around tightly-counted frame flushes — order-dependency surface area is real. Either expose a test-only reset or use `vi.resetModules()` at the suite boundary.
- [ ] P2: Cover overview activation cancellation when the controller unmounts mid-pending:
  mount an overview-controller harness, flush one or two frames so a queued second-rAF or FIFO task is alive, then unmount and flush the remaining frames asserting that `setIsRailReady(true)` was never observed and the FIFO is empty. The current test never unmounts mid-pending so the rAF cancellation paths and FIFO splice-by-task-id are unexercised.
- [ ] P2: Split the multi-purpose marker-menu `AgentSessionPanel` test into focused cases:
  `AgentSessionPanel.test.tsx:646-811` now covers six distinct behaviors (clamp, ArrowDown nav, Escape close, scroll preservation, `.message-meta`-only opening, create/remove). A regression in any earlier step short-circuits later assertions, making failure messages diagnose poorly. Split into focused `it(...)` cases.
- [ ] P2: Narrow `getBoundingClientRect` global spy in marker-menu clamp test:
  `AgentSessionPanel.test.tsx:719-748` mocks `HTMLElement.prototype.getBoundingClientRect` for ALL elements (returning width=0 for everything that isn't the menu) for the lifetime of the try block. Future code that reads bounding boxes during the spy window inherits zero rects silently. Either narrow the spy via a refs/selector check, or scope the mock to a smaller window.
- [ ] P2: Cover the marker-menu clamp offsetWidth/offsetHeight fallback path:
  `clampConversationMarkerContextMenuPosition` falls back to `menu.offsetWidth/offsetHeight` when `getBoundingClientRect` returns zeros (the JSDOM-default case). The existing clamp test mocks the rect to non-zero, exercising only the rect path. Add a test that seeds `offsetWidth`/`offsetHeight` via `Object.defineProperty(HTMLElement.prototype, ...)` and asserts the fallback clamp produces the same expected position.
- [ ] P2: Rename or document `delegation_metadata_size_errors_precede_agent_readiness_setup_errors`:
  with round 52's swap from PATH mutation to `test_agent_setup_failures` injection, the test no longer specifically about *agent readiness setup errors*; it's about "any setup-failure that reaches the bad_request branch after `has_test_runtime_override` is false". Either keep the name and add a comment, or rename to reflect the injected-failure proof, e.g., `delegation_metadata_size_errors_precede_setup_failure_injection`.
- [ ] P2: Add Rust unit tests for `src/telegram_settings.rs`:
  cover `validate_telegram_config` (auto-subscribe of `default_project_id`, auto-fill of `default_project_id` from `default_session_id`, unknown project rejection, unknown session rejection, session-belongs-to-different-project rejection, no-project-on-session rejection); cover `sanitize_telegram_config_for_current_state` (cross-project default-session clearing branch); cover `mask_telegram_bot_token` (empty, all-whitespace, < 8 chars, unicode, full-length token); cover `normalize_project_id_list` (dedup with whitespace, empty filtering); add a router-level test that hits each new `/api/telegram/...` route and asserts JSON shape.
- [ ] P2: Add compact `ConversationOverviewRail` accessibility coverage:
  assert the compact rail exposes an operable role, accessible name, and current/position state instead of only a `navigation` landmark with hidden segment semantics.
- [ ] P2: Add `ConversationOverviewRail` compact-mode keyboard navigation coverage:
  cover `Enter`, `Space`, `ArrowDown`, `ArrowUp`, `Home` in compact mode (the existing `End` test only covers one key path). The `Enter`/`Space` path goes through `navigateToSegmentIndex(currentIndex)` and is meaningfully different from the arrow-key path; the arrow-key path resolves the *current* segment from `viewportProjection.viewportTopPx`. Cover the `viewportProjection.viewportTopPx → currentItem → currentIndex → findOverviewSegmentIndexForItemIndex` chain so a regression that broke it is caught.
- [ ] P2: Pin the `ConversationOverviewRail` compact-vs-per-segment threshold boundary:
  the threshold is hard-coded at 160 (`CONVERSATION_OVERVIEW_COMPACT_SEGMENT_THRESHOLD`). The existing test covers `5` segments (per-segment) and `220 commandMessages` (compact), but `buildConversationOverviewSegments` collapses same-kind runs so 220 messages can produce far fewer segments. Construct messages designed to produce exactly 160 vs. 161 distinct segments and assert per-segment vs. compact rendering at the boundary.
- [ ] P2: Cover the `ConversationOverviewRail` pointer cancellation path:
  the rail's `pointerdown → pointercancel` sequence is uncovered. `finishRailDrag` resets `suppressNextClickRef = false` for `pointercancel`, but no test exercises the cancel branch. On touch hardware, browsers fire `pointercancel` when a long-press triggers a context menu or scroll gesture; a regression that mishandled cancel would silently swallow the user's next click on a segment.
- [ ] P2: Cover the `ConversationOverviewRail` zero-height viewport floor:
  `Math.max(8, viewportProjection.viewportHeightPx)` at line 313 protects the visual indicator from disappearing on extreme zoom. Add a `viewportHeightPx: 0` snapshot rerender and assert `height: "8px"` on the indicator.
- [ ] P2: Cover the `setTimeout` fallback in `scheduleConversationOverviewIdleCallback`:
  the existing `conversation-overview-controller.test.tsx` harness installs an idle-callback shim. The new `scheduleConversationOverviewIdleCallback` (`conversation-overview-controller.ts:96-119`) has a `setTimeout` fallback for environments without `requestIdleCallback` (Safari, embedded webviews) that is uncovered. Add a second test that explicitly deletes `window.requestIdleCallback` and `window.cancelIdleCallback` before render, advances `setTimeout` with `vi.useFakeTimers()`, and asserts the rail still becomes ready after the fallback delay.
- [ ] P2: Tighten the long-active-session `VirtualizedConversationMessageList` test:
  the new "mounts a long active transcript from the estimated bottom window" test asserts `slot count <= 16`. Add a tighter lower bound (e.g., `>= 4`) or assert `getLayoutSnapshot().mountedPageRange.startIndex > 0` so a regression returning an empty mounted range is caught.
- [ ] P2: Restructure the `bottom_pin` mount test to exercise unmount→mount transition:
  the new "bottom-pin mounts the bottom range without starting a boundary reveal" test (`VirtualizedConversationMessageList.test.tsx:572-600`) asserts the bottom message renders, but `getByText("message-48")` was already true before the `notifyMessageStackScrollWrite` because `waitFor` ran first. Restructure: scroll to top so message-48 is unmounted, dispatch `bottom_pin`, then assert message-48 mounts AND the boundary-reveal attribute is absent — exercising both halves of the contract.
- [ ] P2: Prove deferred-tail-window initial render starts from newest messages:
  extend `AgentSessionPanel.test.tsx:1493-1525` so the first render includes a newest-tail message and excludes an early message, then assert the early/full transcript appears only after the scheduled hydration delay.
- [ ] P2: Add intermediate-checkpoint assertion to the deferred-tail-window test:
  `AgentSessionPanel.test.tsx:1493-1525` asserts only that the rail is absent immediately and present after 1.5s of advanced timers. A regression that made the rail render *immediately* (defeating the deferral) would still pass the post-advance assertion. Add an intermediate `act(() => vi.advanceTimersByTime(100))` checkpoint asserting the rail is *still* absent. Or assert directly on a transcript-hydration symptom (count of mounted message slots is small at t=0 and large at t=1500ms).
- [ ] P2: Add `api.ts` Telegram client coverage:
  `fetchTelegramStatus`, `updateTelegramConfig`, `testTelegramConnection` are not exercised by `api.test.ts`. Mirror the existing `createOrchestratorInstance` test pattern to pin URL + method + JSON body for at least `updateTelegramConfig`. Catches typos like `/api/telegram/conifg`.
- [ ] P2: Add direct test file for `ui/src/delegation-error-packets.ts`:
  `SAFE_SPAWN_DELEGATION_PATTERNS` regex set is exercised only indirectly through `delegation-commands.test.ts`. A typo in `(?:a drive-relative...|a UNC...|a Windows device namespace...)` would only fail if the integration test happens to surface that specific message. Add `delegation-error-packets.test.ts` with `it.each` over each safe message + each pattern, plus an unsafe-message control case asserting the generic fallback.
- [ ] P2: Cover roving tabindex update in `ConversationOverviewRail`:
  the per-segment buttons render with `tabIndex={index === 0 ? 0 : -1}` regardless of focus. Add a test that focuses a non-first segment via keyboard, then blurs and refocuses the rail, and asserts focus restores to the previously-interacted segment (currently fails because tabIndex is purely a function of index).
