# Bugs & Known Issues

Updated against the current checked-in code in `src/main.rs`, `ui/src/App.tsx`,
`ui/package.json`, and `ui/vite.config.ts`.

Completed items are removed from this file once fixed. The sections below track
active bugs and follow-up tasks only.

The `objectHasOwnWithFallback` test was promoted to a top-level
`describe("objectHasOwnWithFallback")` block in `OrchestratorTemplatesPanel.test.tsx`, decoupling
it from the component suite's `beforeEach`/`afterEach` hooks it never needed.

## `null` transition anchors survive the `readState` restore boundary as `null` instead of absent

**Severity:** Low - a subtle type-contract mismatch between the localStorage deserialization guard and the TypeScript type for `OrchestratorTemplateTransition`.

`isTransitionTemplate` correctly allows `null` for `fromAnchor`/`toAnchor` (meaning "compute nearest anchor at render time"), and the restore spread preserves that `null` verbatim: `...(transition.fromAnchor !== undefined ? { fromAnchor: transition.fromAnchor } : {})`. If `OrchestratorTemplateTransition.fromAnchor` is typed as `AnchorSide | undefined` — not `| null` — a `null` anchor from localStorage survives the validation boundary as `null` rather than being normalized to absent (`undefined`). Call sites that check `=== undefined` to detect "no anchor" will see a `null` and behave incorrectly.

**Current behavior:**
- `isTransitionTemplate` passes `null` anchors through the guard
- the restore spread copies `null` into draft state unchanged
- downstream code that branches on `anchor === undefined` vs. a real `AnchorSide` sees `null` and may take an unexpected path

**Proposal:**
- change the spread condition from `!== undefined` to `!= null` so both `null` and `undefined` anchors are normalized to absent: `...(transition.fromAnchor != null ? { fromAnchor: transition.fromAnchor } : {})`

## `AGENT_OPTIONS` arrays are entry-validated but not exhaustive against the `AgentType` union

**Severity:** Low - `as const satisfies ReadonlyArray<{ label: string; value: AgentType }>` validates that each existing entry's `value` is a known `AgentType`, but it does not enforce that every `AgentType` has an entry. A new agent added to the union will not appear in either the new-session dropdown (`NEW_SESSION_AGENT_OPTIONS` in `App.tsx`) or the orchestrator template agent picker (`AGENT_OPTIONS` in `OrchestratorTemplatesPanel.tsx`) without a compile-time error.

By contrast, `SUPPORTED_PERSISTED_TEMPLATE_AGENTS satisfies Record<AgentType, true>` is exhaustive and will catch a new variant immediately.

**Current behavior:**
- `AGENT_OPTIONS` and `NEW_SESSION_AGENT_OPTIONS` use `satisfies ReadonlyArray<...>` which validates existing entries only
- adding a new `AgentType` variant produces no TypeScript error on either dropdown array
- `SUPPORTED_PERSISTED_TEMPLATE_AGENTS` is exhaustive but is only used for the restore guard, not the UI dropdowns

**Proposal:**
- derive a companion `satisfies Record<AgentType, boolean>` map from each options array (or add a static assertion that the union of `value` fields covers all `AgentType` members), so TypeScript errors when a new agent is added to the union but not to the dropdown

## `pendingTransitions: []` in test fixtures exercises a path absent from real SSE payloads

**Severity:** Low - test mock objects that construct `OrchestratorInstance` still pass `pendingTransitions: []` (the old required-field form). The Rust backend uses `#[serde(skip_serializing_if = "Vec::is_empty")]`, so the field is absent from any SSE payload or API response when the list is empty. Fixtures with `pendingTransitions: []` exercise the `length === 0` path rather than the `=== undefined` path, meaning any frontend code that branches on `pendingTransitions !== undefined` behaves differently against real data vs. test fixtures.

**Current behavior:**
- `OrchestratorInstance` mock objects in `App.test.tsx` and `OrchestratorTemplatesPanel.test.tsx` include `pendingTransitions: []`
- real backend payloads with an empty list omit the field entirely
- frontend code that guards `instance.pendingTransitions !== undefined` before iterating will see different behavior in tests vs. production

**Proposal:**
- remove `pendingTransitions: []` from all mock objects that construct `OrchestratorInstance`; use `instance.pendingTransitions ?? []` at call sites that need to iterate the value

## Inline streaming-tail reconcile test missing `toEqual` value-equality assertion

**Severity:** Low - the pre-existing "reuses unchanged messages when only the streaming tail message changed" test in `session-reconcile.test.ts` received `expect(merged[0].messages[1]).toBe(next[0].messages[1])` (reference identity) but has no `toEqual` assertion confirming the content of the changed message. A reconciler that returns the correct reference but with wrong content would still pass.

The new `expectChangedMessageReference` helper includes `expect(merged[0].messages[1]).toEqual(nextMessage)` as a sixth assertion; the inline test should match that stricter pattern.

**Current behavior:**
- the inline test asserts the changed message is `!== previous` and `=== next` but not that it `equals` the expected content
- a content-corrupting reconciler regression would produce a false pass on this test

**Proposal:**
- add `expect(merged[0].messages[1]).toEqual(next[0].messages[1])` after the `toBe` assertion, consistent with `expectChangedMessageReference`

## `ui/src/App.tsx` is large enough to trigger Babel deoptimization warnings

**Severity:** Note - the main frontend entry file is now large enough to create tooling friction even though the app still runs correctly.

Running the frontend now emits Babel's "code generator has deoptimised the styling" warning for
`ui/src/App.tsx` because the file exceeds the generator's 500 KB pretty-print threshold. This is not
an immediate runtime bug, but it is a concrete signal that build output, source-map ergonomics, and
routine maintenance are all getting worse as more UI behavior accumulates in one file.

The underlying problem is structural rather than cosmetic: workspace layout logic, pane routing,
control-surface state, session rendering, and modal/settings behavior all continue to land in the
same top-level module. That raises the cost of review and makes unrelated UI changes collide in the
same file more often than they should.

**Current behavior:**

- `ui/src/App.tsx` is large enough to trigger Babel's deoptimization warning during frontend builds
- multiple distinct UI responsibilities still live in one monolithic module
- routine frontend changes frequently require editing or reviewing the same oversized file

**Proposal:**

- split `ui/src/App.tsx` into smaller modules by responsibility instead of continuing to grow the main file
- extract self-contained workspace/controller hooks and pane-rendering sections first, where the existing seams already exist
- keep `App.tsx` as the composition/root wiring layer instead of the implementation home for every control flow

## P2

- [ ] Expand HTTP route tests for the axum API:
      Codex thread actions (archive, unarchive, fork, rollback) and interactive request submissions
      (user input, MCP elicitation, generic app requests) now have HTTP route tests via
      `tower::ServiceExt`. Still missing: session creation, message send, settings updates, Claude
      approvals, and SSE state events.
- [ ] Add backend regression tests for divergent completed-text replacement in Codex streaming:
      cover both shared-Codex and REPL-Codex paths where the final authoritative text must replace,
      not append to, previously streamed content.
- [ ] Add orchestrator lifecycle endpoints (stop, pause, resume):
      `OrchestratorInstanceStatus` defines `Running`, `Paused`, and `Stopped` but no API endpoint
      transitions between them. Users cannot stop a running orchestration except by killing
      individual sessions.
- [ ] Add orchestrator instances to `StateResponse` or a dedicated SSE delta:
      the frontend currently has no push notification for orchestrator state changes (transitions
      fired, instances completed). It must poll `GET /api/orchestrators`.
- [ ] Add App-level regression coverage for control-surface tab selection sync:
      render a docked control panel plus standalone git/files panes with neighboring sessions, then
      verify selecting the control surface adopts the nearest session context instead of leaving stale
      origin-based project state behind.
- [ ] Add App-level control-surface launch regressions for pane-local Files/Git roots:
      render split panes with different session contexts, then verify Files/Git launchers and panels
      use a root/workdir that matches the same pane-local session/project metadata they emit.
- [ ] Add a canvas-move regression for post-relocation session/project sync:
      when an existing shared canvas is moved into a new pane, assert its origin metadata and the
      target pane's `activeSessionId` are updated to the new launch context instead of the old one.
- [ ] Verify/fix Codex `thread/resume` safety after failed `turn/interrupt`:
      `stop_session_dispatches_queued_prompt_after_shared_codex_interrupt_failure` dispatches the
      queued prompt with `resume_thread_id` pointing to a thread whose `turn/interrupt` returned
      an error. If the prior turn is still running in Codex, `thread/resume` + `turn/start` may
      produce interleaved turns; the resumed completion arrives on the shared reader with no session
      mapped (detached), so it is silently dropped. Verify against the Codex JSON-RPC spec and, if
      unsafe, fall back to `thread/start` (new thread) when the interrupt fails.
- [ ] Add unit tests for orchestrator geometry functions:
      `anchorPosition`, `nearestAnchorSide`, `nearestAnchorPosition`, `buildTransitionGeometry`,
      `buildSelfLoopTransitionGeometry`, `anchorNormal`, `cubicBezierPoint`, `cubicBezierDerivative`,
      `perpendicularOffsetPoint`, and `isValidAnchor` are pure deterministic functions with no DOM
      dependencies.
- [ ] Continue splitting backend modules as they grow:
      `src/main.rs` was split into `api.rs`, `state.rs`, `runtime.rs`, `turns.rs`, `remote.rs`, and
      `tests.rs`. Some of these modules (especially `state.rs` and `turns.rs`) are already large and
      could benefit from further decomposition as features stabilize.
- [ ] Add frontend test for `createOrchestratorInstance` with omitted `projectId`:
      the backend has two new tests (route-level and state-level) verifying the template-project
      fallback when `projectId` is absent; no frontend test verifies that the
      `...(projectId ? { projectId } : {})` conditional spread in `api.ts` actually fires
      when the panel has no project selected. Add a test where `OrchestratorTemplatesPanel` runs
      a template with `projectId: null` and asserts `createOrchestratorInstanceMock` was called with
      only `templateId` (no second argument or `projectId: undefined`).
- [ ] Add HTTP-boundary test for `POST /api/orchestrators` with explicit `"projectId": ""`:
      the route test omits `projectId` entirely from the JSON body; a client sending
      `{ "templateId": "...", "projectId": "" }` takes a different serde path
      (`Some("")` → trim → filter → template fallback) that is correct but untested. Add a
      `request_json` test posting the explicit empty-string body and asserting the template fallback fires.
- [ ] Migrate remaining `EventSourceMock.instances[0]` uses to stale-safe pattern:
      integration tests outside the two refactored drag-drop tests still access
      `EventSourceMock.instances[0]` (hardcoded index). If any earlier test leaves stale instances in
      the array, those tests operate on the wrong `EventSource`. Capture `priorEventSourceCount`
      before calling `renderApp()` and use `EventSourceMock.instances[priorEventSourceCount]`,
      or migrate to `renderAppWithProjectAndSession`.
- [ ] Fix `null` anchor passthrough in `readState` transition restore:
      change `!== undefined` to `!= null` in the `fromAnchor`/`toAnchor` conditional spreads
      so `null` anchors are normalized to absent rather than propagated into draft state as `null`,
      matching `OrchestratorTemplateTransition`'s TypeScript type (`AnchorSide | undefined`).
- [ ] Add `AgentType` exhaustiveness check to `AGENT_OPTIONS` arrays:
      derive a companion `satisfies Record<AgentType, boolean>` map from `AGENT_OPTIONS` and
      `NEW_SESSION_AGENT_OPTIONS` so TypeScript errors when a new agent is added to the union but
      not to either dropdown, matching the exhaustiveness already enforced by
      `SUPPORTED_PERSISTED_TEMPLATE_AGENTS satisfies Record<AgentType, true>`.
- [ ] Add `toEqual` assertion to inline streaming-tail reconcile test:
      "reuses unchanged messages when only the streaming tail message changed" asserts
      `merged[0].messages[1]` is `=== next[0].messages[1]` but not that the content is correct;
      add `expect(merged[0].messages[1]).toEqual(next[0].messages[1])` to match the
      stricter pattern enforced by `expectChangedMessageReference`.
- [ ] Fix `pendingTransitions: []` in `OrchestratorInstance` test fixtures:
      mock objects that include `pendingTransitions: []` exercise a path the backend never
      produces (the field is omitted when empty via `skip_serializing_if`). Remove the field
      from mock construction; add `?? []` at the few call sites that need to iterate it so
      tests accurately reflect the real SSE wire format.

## Later
