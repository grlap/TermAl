# Bugs & Known Issues

This file tracks reproduced, current issues. Resolved work, speculative refactors,
and cleanup notes do not belong here.

## Active Repo Bugs

### ~~`StateResponse.codex` and `agentReadiness` still typed as optional in TypeScript~~ - FIXED

Made `codex`, `agentReadiness`, and `preferences` required in `ui/src/api.ts`, then updated the typed frontend fixtures that construct `StateResponse` values (`ui/src/App.test.tsx`, `ui/src/backend-connection.test.tsx`, `ui/src/panels/OrchestratorTemplateLibraryPanel.test.tsx`, and `ui/src/panels/OrchestratorTemplatesPanel.test.tsx`) so the compile-time contract now matches the always-emitted backend shape.

### ~~`docs/architecture.md` missing status-code and response-type annotations for new orchestrator endpoints~~ - FIXED

Annotated `DELETE /api/orchestrators/templates/{id}` with `(200) -> OrchestratorTemplatesResponse` plus its full-list-return intent, and added `-> StateResponse` to the orchestrator pause/resume/stop rows in `docs/architecture.md`.

### ~~Revision gate TOCTOU~~ — FIXED

Moved `note_remote_applied_revision` before `commit_locked` at all 4 call sites
(`create_remote_session_proxy`, `create_remote_orchestrator_proxy`,
`proxy_remote_fork_codex_thread`, kill/remove) in `src/remote.rs`. The revision gate
is now closed before SSE emission, eliminating the concurrent double-apply window.

### ~~Workspace delete error path spinner~~ — FIXED

Added `setIsWorkspaceSwitcherLoading(false)` to the `catch` block of
`handleDeleteWorkspace` in `ui/src/App.tsx`.

### ~~`refreshWorkspaceSummaries` useEffect dependency~~ — FIXED

Wrapped `refreshWorkspaceSummaries` in `useCallback` (all captured values are stable
refs/setters) and added it to the `useEffect` dependency array in `ui/src/App.tsx`.

### ~~Hard-coded developer paths in test fixture~~ — FIXED

Replaced `C:/Users/grzeg/...` paths with `/home/testuser/...` in
`ui/src/App.test.tsx` Gemini warning test.

### ~~Workspace-delete test localStorage cleanup~~ — FIXED

Added `window.localStorage.removeItem(WORKSPACE_LAYOUT_STORAGE_KEY:monitor-left)`
to the `finally` block in `ui/src/App.test.tsx`.

### ~~TOCTOU fix completed~~ — FIXED

Moved `note_remote_applied_revision` before `commit_locked` in the remaining 3 sites:
`sync_remote_state_for_target`, `proxy_remote_orchestrator_state_action`, and
`apply_remote_state_snapshot`. All call sites in `src/remote.rs` now note the revision
before emitting SSE.

### ~~Delta event `>=` predicate silently drops same-revision events~~ - FIXED

Added `should_skip_remote_applied_delta_revision` in `src/state.rs` and wired every
`apply_remote_delta_event` branch in `src/remote.rs` to use strict `>` semantics for
deltas while keeping `>=` for snapshot paths. Added regression coverage for both the
helper semantics and a same-revision remote delta sequence.

### ~~`handleDeleteWorkspace` discards token, races with in-flight refresh~~ - FIXED

Captured the request token returned by `beginWorkspaceSummariesRequest()` in both the
success and error paths of `handleDeleteWorkspace` in `ui/src/App.tsx`, and now only
clear `isWorkspaceSwitcherLoading` when that token is still the latest request.

### ~~Raw remote error body forwarded verbatim to the browser~~ - FIXED

Added `sanitize_remote_error_body` in `src/remote.rs` so non-JSON remote error bodies
are whitespace-normalized, stripped of control characters, and capped at 512
characters before they become `ApiError` messages. Added backend coverage for the
sanitization and truncation behavior.

### ~~Gemini settings precedence reversed~~ — FIXED

Reversed the iteration order in `gemini_selected_auth_type` and
`gemini_interactive_shell_setting` (`src/runtime.rs`): system settings are now checked
first (override), then user, then project (fallback). Matches Gemini's documented
"system settings override other settings" semantics.

### ~~Scroll regression for partially visible messages~~ — FIXED

Changed `getAdjustedVirtualizedScrollTopForHeightChange` to only adjust `scrollTop`
when the message is fully above the viewport (`messageTop + previousHeight <= scrollTop`).
Partially visible messages now grow in-place without jumping the viewport. Added 2 new
test cases covering the partially-visible and boundary scenarios.

### ~~Gemini tests not isolated from machine credentials~~ — FIXED

`gemini_interactive_shell_warning_respects_workspace_settings` and
`gemini_invalid_session_load_falls_back_to_session_new` both failed on machines where
TermAl's system settings override (`C:\ProgramData\gemini-cli\settings.json`) existed
or where `GEMINI_API_KEY` was not set.

- `gemini_interactive_shell_warning_respects_workspace_settings`: now holds
  `TEST_HOME_ENV_MUTEX`, redirects `GEMINI_CLI_SYSTEM_SETTINGS_PATH` to an absent path,
  and redirects `USERPROFILE` to an empty temp home so real system/user settings cannot
  shadow the project-level value under test.
- `gemini_invalid_session_load_falls_back_to_session_new`: now holds
  `TEST_HOME_ENV_MUTEX` and sets `GEMINI_API_KEY=test-key-not-real` for the duration of
  the test so `validate_agent_session_setup` passes without real credentials. Added
  `ScopedEnvVar::set` helper to support non-path string env-var overrides.

### ~~`sanitize_remote_error_body` Unicode panic~~ — FIXED

Replaced byte-offset `truncate(509)` with `char_indices().nth(509)` to find a safe
char-boundary byte offset. Multi-byte characters (emoji, CJK) no longer panic.

### ~~`handleDeleteWorkspace` double request token~~ — FIXED

Moved `beginWorkspaceSummariesRequest()` before the `try/catch` block so a single
token is shared between success and error paths. Concurrent in-flight refreshes are
no longer invalidated by a failed delete.

### ~~`setWorkspaceSummaries` filter token-guard regression~~ — FIXED

Round 3 moved the `setWorkspaceSummaries(current => current.filter(...))` call inside
the `isLatestWorkspaceSummariesRequest(requestToken)` guard. That broke overlapping
concurrent deletes: if delete A (token 1) resolved after delete B (token 2), the token
check failed and A's workspace was never removed from the list.

Correct behaviour: the filter is unconditional — each delete mutates only its own
distinct id and must always apply when the component is mounted. Only
`setIsWorkspaceSwitcherLoading(false)` stays token-guarded, so a stale delete cannot
prematurely clear the spinner while a newer request is still in flight.

### ~~Partially-visible shrinking message scroll test~~ — FIXED

Added test case: `messageTop=50, previousHeight=100, nextHeight=60,
currentScrollTop=100` → returns `60`.

### ~~`create_remote_orchestrator_proxy` makes two sequential blocking HTTP calls on the error path~~ - FIXED

Cached `supports_inline_orchestrator_templates` on `RemoteConnection` from the health check and reused that cache in `create_remote_orchestrator_proxy`, so the old-remote upgrade path no longer performs a second blocking `GET /api/health` call. Added a regression test that asserts the request sequence stays at one health probe plus the orchestrator create attempt.

### ~~`adoptState` workspace-switcher reset~~ - FIXED

Guarded `setWorkspaceSummaries` in `adoptState` with `nextState.workspaces !== undefined` so non-workspace API responses (orchestrator actions, etc.) no longer wipe the switcher.

### ~~`deleteWorkspaceLayout` response discarded~~ - FIXED

`handleDeleteWorkspace` now uses the authoritative `WorkspaceLayoutsResponse.workspaces` returned by `DELETE /api/workspaces/{id}` instead of a local filter, correctly reflecting concurrent cross-tab operations.

### ~~Brittle error-message substring match used as remote capability gate~~ - FIXED

The upgrade gate now checks HTTP 404 plus the cached inline-template capability bit instead of matching human-readable error text, so remote wording changes no longer suppress the actionable upgrade message.

### ~~Orchestrator `aria-label` disambiguation~~ - FIXED

Added `aria-label={`Pause/Resume/Stop orchestration ${entry.orchestrator.id}`}` to all four orchestrator action buttons. Tests updated to match via `/^Pause orchestration/` regex.

### ~~`local_project_id.unwrap_or_default()` silently assigns empty `project_id`~~ - FIXED

`localize_remote_orchestrator_instance` now returns an error when no local project mapping exists instead of manufacturing an empty project id. Added a regression test covering an unmapped remote project snapshot.

### ~~SSE bridge loop silently drops connection error~~ - FIXED

Both the remote availability failure path and the initial `GET /api/events` connect failure path now log the connect error before retrying, making repeated remote bridge failures visible in stderr.

### ~~"GitHub:" aria-label prefix is misleading for non-GitHub repos~~ - FIXED

Git tabs now use `Git status: <path>` for their accessible label while keeping the existing icon. Tests were updated to assert the new label.

### ~~`adoptState` wipes projects/orchestrators~~ — FIXED

Added `!== undefined` guards for `projects` and `orchestrators` in `adoptState`, matching
the existing `workspaces` guard. Also removed `skip_serializing_if = "Vec::is_empty"` from
ALL collection fields on `StateResponse` in `src/api.rs` so the backend always emits them
(including as `[]`), which simultaneously fixes the SSE empty-workspace broadcast blind spot.

### ~~SSE empty-workspace broadcast swallowed~~ — FIXED

Resolved by the `skip_serializing_if` removal above: the backend now always includes
`"workspaces": []` in SSE snapshots, so the `!== undefined` guard correctly applies the
empty list instead of ignoring it.

### ~~`OrchestratorTemplatesPanel` raw `"local"` string~~ — FIXED

Replaced `project.remoteId !== "local"` with `!isLocalRemoteId(project.remoteId)` in
`OrchestratorTemplatesPanel.tsx`, correctly handling `remoteId = ""`.

### ~~`resolveControlSurfaceSectionIdForWorkspaceTab` non-exhaustive~~ — FIXED

Replaced `default: null` with explicit cases for all 7 tab kinds that return `null`
(`session`, `source`, `controlPanel`, `canvas`, `orchestratorCanvas`,
`instructionDebugger`, `diffPreview`). Adding a new `WorkspaceTab` kind now produces a
TypeScript "not all code paths return a value" error.

### ~~`handleDeleteWorkspace` cleanup duplication~~ — FIXED

Moved `finishDeletingWorkspace` and the token-guarded `setIsWorkspaceSwitcherLoading(false)`
into a `finally` block. `deleteStoredWorkspaceLayout` stays in the try block since it should
only run on success.


### ~~Capability cache `None` falls through to generic error on remote 404~~ - FIXED

The 404 upgrade gate in `create_remote_orchestrator_proxy` now treats cached capability `None` the same as `Some(false)`, so the first old-remote launch after reconnect surfaces the upgrade message instead of a raw "template not found" error. Added coverage for both the cache-miss and pre-cached-`Some(false)` paths.

### ~~`PUT` and `DELETE /api/workspaces/{id}` return different response shapes~~ - FIXED

Documented the intentional asymmetry directly on both backend route handlers and the frontend API helpers: `PUT` returns the saved document, while `DELETE` returns the remaining summary list. Future implementors now have the contract explained at the call sites instead of discovering it indirectly.

### ~~Unbounded memory allocation before `sanitize_remote_error_body`~~ - FIXED

`decode_remote_json` now bounds non-success response reads to 64 KiB before sanitization/JSON parsing and returns `remote error response too large` when the cap is exceeded. Added a regression test covering the oversized-body path.

### ~~`agentReadiness` and `codex` adoptState guards~~ — FIXED

Replaced `nextState.agentReadiness ?? []` and `nextState.codex ?? {}` with
`!== undefined` guards matching the pattern on `projects`, `orchestrators`,
and `workspaces`.

### ~~`StateResponse.sessions` missing `#[serde(default)]`~~ — FIXED

Added `#[serde(default)]` to `sessions` in `StateResponse` (`src/api.rs`),
matching every other collection field.

### ~~`handleDeleteWorkspace` missing loading activation~~ — FIXED

Added `setIsWorkspaceSwitcherLoading(true)` after `beginWorkspaceSummariesRequest()`
and before `try`, so users see loading feedback during deletion.

### ~~TypeScript `StateResponse` optional fields don't match always-emitted backend shape~~ - FIXED

Made `projects`, `orchestrators`, and `workspaces` required in the TypeScript `StateResponse`
shape and updated the typed UI tests to build the full backend-emitted snapshot.

### ~~`AcpRuntimeState` manual Default~~ — FIXED

Replaced the manual `impl Default` with `#[derive(Default)]` on `AcpRuntimeState`, letting
  the compiler keep the impl in sync automatically.

### ~~Remote revision watermarks not cleared synchronously on config change~~ — FIXED

`RemoteConnection::update_config` only called `disconnect()` when a remote config changed,
but did not clear the remote applied revision watermark or SSE fallback resync tracking.
Those clears happened later in the async bridge loop, so a request racing in before the
bridge loop iterated could see the stale watermark and drop the first response from a newly
pointed/restarted remote as "stale."

Changed `update_config` to return `bool` (whether config changed), `reconcile` to return the
IDs of changed/removed remotes, and the `update_app_settings` caller in `src/state.rs` to
synchronously clear both `clear_remote_applied_revision` and `clear_remote_sse_fallback_resync`
for every changed remote ID.

### ~~`handleDeleteWorkspace` unconditionally replaces workspace list, bypassing revision gate~~ — FIXED

`handleDeleteWorkspace` unconditionally applied `deleteResponse.workspaces` from the
`DELETE /api/workspaces/{id}` response, sidestepping the revision-gated `adoptState` path.
If an SSE state snapshot with a newer revision arrived during the in-flight delete (e.g.,
another tab saved/created a workspace), the older delete response would overwrite it.

Both `handleDeleteWorkspace` and `refreshWorkspaceSummaries` now capture
`workspaceSummariesRef.current` before making their request and compare with reference
equality on completion. This workspace-specific staleness check only triggers when the
workspace list was actually updated by another source (SSE-delivered workspace data, another
delete, or a concurrent refresh) — unrelated session/orchestrator SSE events no longer
cause valid responses to be discarded.

For the delete success path the full post-delete list is applied only when this is still the
latest workspace request (`isLatestWorkspaceSummariesRequest`) AND the workspace list has not
been updated during the flight. In all other cases (newer request started, overlapping
delete, or workspace SSE during the flight), the handler falls back to a local filter that
removes only the confirmed-deleted workspace ID, preserving whatever more-authoritative
source populated the rest. The error path checks `isLatestWorkspaceSummariesRequest` so a
stale failed delete does not overwrite error state from a newer in-flight request.

### ~~`refreshWorkspaceSummaries` can overwrite SSE-adopted workspace list~~ — FIXED

`refreshWorkspaceSummaries` applied the `/api/workspaces` response unconditionally once the
request token was still current, but never checked whether the workspace list had been
updated by SSE during the fetch. Since workspace saves now publish fresh state snapshots,
a slower `/api/workspaces` response could hide a workspace that was added or updated by
another tab.

Added a `workspacesAtRequest` capture before the fetch and a reference equality check on
success. When the workspace list has been updated during the fetch (by SSE or another
handler), the stale response is discarded. Unrelated session/orchestrator SSE events no
longer cause the refresh result to be dropped.

### ~~`adoptState` skip-if-unchanged optimization is a no-op inside the SSE effect~~ - FIXED

`adoptState` now compares against ref-backed baselines (`codexStateRef`, `agentReadinessRef`, `projectsRef`, `orchestratorsRef`, `workspaceSummariesRef`) instead of stale render-closure snapshots, so the long-lived SSE effect benefits from the same skip-if-unchanged behavior as synchronous callers.

### ~~Double rollback in `create_remote_orchestrator_proxy`~~ - FIXED

Removed the outer rollback snapshot from `create_remote_orchestrator_proxy` and now rely on `ensure_remote_orchestrator_instance`'s internal rollback when localization fails, eliminating the redundant double-restore path.

### ~~Manual JSON construction for remote orchestrator request~~ - FIXED

`create_remote_orchestrator_proxy` now builds a typed `CreateOrchestratorInstanceRequest` and serializes it with `serde_json::to_value`, so request field names stay aligned with the struct's serde annotations.

### ~~Unsanitized remote `ErrorResponse.error` forwarded to clients~~ - FIXED

Structured remote JSON errors now pass through `sanitize_remote_error_body` before becoming `ApiError` messages, giving `{ "error": "..." }` responses the same whitespace normalization, control-character stripping, and 512-character cap as raw-body fallbacks. Added a backend regression covering the structured-error path.

### ~~Missing blank line before SSE heading in architecture.md~~ - FIXED

Added the missing blank line before `### SSE Event Stream` in `docs/architecture.md`.

### ~~Gemini warning test synchronous assertion after `waitFor` may flake~~ - FIXED

Moved the Gemini readiness-detail assertion into the existing `waitFor` block in `ui/src/App.test.tsx`, so both the warning text and the readiness text now wait on the same async UI settlement.

### ~~Gemini and model-options tests use old `scrollIntoView` mock pattern~~ - FIXED

The two missed `ui/src/App.test.tsx` call sites were already converted to the shared `stubScrollIntoView()` helper with `.mockRestore()` cleanup. This entry was stale and is now marked resolved.

### ~~Duplicated recorder-state clearing logic in `turns.rs` and `runtime.rs`~~ - FIXED

Extracted shared `reset_recorder_state_fields(&mut SessionRecorderState)` logic and now route both `recorder_reset_turn_state` and `clear_shared_codex_turn_recorder_state` through that helper, eliminating the duplicate field-reset implementation.

### ~~`String(agent)` in `agent-icon.tsx` defeats exhaustiveness checking~~ - FIXED

`ui/src/agent-icon.tsx` now keeps `agent` typed as `AgentType` and renders via an exhaustive `switch`. The fallback path remains runtime-safe for invalid cast inputs, while TypeScript regains exhaustiveness checking for future agent variants.

## Known External Limitations

No currently tracked external limitations require additional TermAl changes.
Windows-specific agent caveats are now handled in-product:
- TermAl forces Gemini ACP sessions to use `tools.shell.enableInteractiveShell = false`
  through a TermAl-managed system settings override on Windows.
- TermAl surfaces WSL guidance before starting Codex on Windows because upstream
  PowerShell parser failures still originate inside Codex itself.

## Implementation Tasks

### ~~Gemini dotenv key-based lookup~~ - FIXED

Changed `dotenv_var_value`, `dotenv_var_source`, and `gemini_dotenv_env_pairs` to search all candidate env files (`~/.gemini/.env`, `~/.env`) per-key instead of using "first file wins." A user who keeps Gemini flags in `~/.gemini/.env` and API keys in `~/.env` now gets correct readiness detection and child-env injection.

### ~~Audit `remote_same_revision_deltas_apply_in_sequence` second-delta assertion~~ - FIXED

Confirmed the behavior is intentional. When a remote `CommandUpdate` arrives for a missing command message, `apply_remote_delta_event` creates that message locally and publishes a normalized `MessageCreated` delta. Added an inline comment in the test to make the expected variant explicit.

### ~~Add workspace delete failure-path test and in-flight disabled-button test~~ - FIXED

Added focused UI coverage for both failure recovery and the in-flight disabled state in `ui/src/App.test.tsx`.

### ~~Add unit tests for `formatVisibleTabLabel` and `resolveControlSurfaceSectionIdForWorkspaceTab`~~ - FIXED

Exported both helpers and added focused assertions for their mapped cases in `ui/src/panels/PaneTabs.test.tsx` and `ui/src/App.test.tsx`.

### ~~Add test for `Some(false)` pre-cached capability path in `create_remote_orchestrator_proxy`~~ - FIXED

Added a Rust regression that seeds `supports_inline_orchestrator_templates = Some(false)` before launch and verifies the old-remote upgrade message still appears without an extra post-404 health probe.

### ~~Replace raw localStorage key inspection in `handleDeleteWorkspace` test with a spy~~ - FIXED

The delete-success UI test now spies on `workspaceStorage.deleteStoredWorkspaceLayout` instead of inspecting the raw `localStorage` key directly.

### ~~Fix self-contradicting assertion in `remote_orchestrator_create_requires_upgrade_when_inline_template_support_is_precached_false`~~ - FIXED

Aligned the test comments and assertions around the intended `Some(false)` behavior: one normal
availability probe before launch, and no extra health probe after the remote returns 404.

### ~~Add `adoptState` partial-response guard tests~~ - FIXED

Added focused `resolveAdoptedStateSlices` coverage for omitted `projects`, `orchestrators`, and
`workspaces`, plus explicit empty replacements for `agentReadiness` and `codex` after the guard
conversion to `!== undefined` checks.

### ~~Add `objectHasOwnWithFallback` native-path test~~ - FIXED

Added the native `Object.hasOwn` path coverage in `ui/src/panels/OrchestratorTemplatesPanel.test.tsx`, alongside the existing fallback-path assertion.

### ~~Add unit test for `saveFile` conditional-spread behavior~~ - FIXED

Added `saveFile` coverage in `ui/src/api.test.ts` that passes `scope = { sessionId: "" }` and verifies the serialized request body omits `sessionId`.

### ~~Fix `EventSourceMock.instances` double-splice in stale-instance test~~ - FIXED

Updated the stale-instance test in `ui/src/App.test.tsx` so the outer `finally` trims `EventSourceMock.instances` before the helper cleanup runs, eliminating the double-splice ordering bug.

### ~~Replace `HTMLElement.prototype.scrollIntoView = vi.fn()` with `vi.spyOn` in test helper~~ - FIXED

`renderAppWithProjectAndSession` in `ui/src/App.test.tsx` now uses `vi.spyOn(HTMLElement.prototype, "scrollIntoView")` and restores that spy during helper cleanup, so `vi.restoreAllMocks()` remains a reliable fallback.

### ~~Replace remaining soft-assert `expect(x).toBeTruthy()` patterns in `ui/src/workspace.test.ts`~~ - FIXED

Replaced the two remaining soft assertions in `ui/src/workspace.test.ts` with explicit throw-guards for missing `sourcePane` and `diffPane`, then used the unwrapped `.id` values directly.

- [x] Add unit tests for `buildControlSurfaceSessionListEntries` and `formatSessionOrchestratorGroupName` - FIXED:
  Exported both functions from `ui/src/App.tsx` and added focused tests for standalone sessions, blank-name fallback to `templateId`, newest-orchestrator wins for multiply-referenced sessions, and the empty-session case.

- [x] Add test for `handleOrchestratorRuntimeAction` error path - FIXED:
  Expanded the grouped-session orchestrator action error test to assert both the surfaced error message and that the action buttons are re-enabled after a failed pause request.

- [x] Remove dead `expect(eventSource).toBeTruthy()` assertions in `ui/src/App.test.tsx` - FIXED:
  Removed the redundant guards. `latestEventSource()` remains the single null-check and now throws directly when no mock instance exists.

- [x] Migrate remaining `scrollIntoView` mocks to `vi.spyOn` pattern in `ui/src/App.test.tsx` - FIXED:
  The previously missed Gemini warning and model-options tests now also use the shared `stubScrollIntoView()` helper with `.mockRestore()` cleanup, so the file is consistent on the safer spy-based pattern.

- [x] Wrap Gemini warning test readiness assertion in `waitFor` - FIXED:
  The Gemini warning test now asserts the readiness detail inside the same `waitFor` block as the warning text, removing the last synchronous post-`waitFor` readiness check.

- [x] Add unit test for `deleteWorkspaceLayout` in `ui/src/api.test.ts` - FIXED:
  Added focused API-module coverage for the `DELETE` request shape, `encodeURIComponent` path handling,
  and `WorkspaceLayoutsResponse` parsing in `ui/src/api.test.ts`.
