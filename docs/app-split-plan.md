# App Split Plan

## Goal

Split the oversized frontend entrypoint and its monolithic integration test file into smaller, reviewable modules without changing behavior.

Current state:
- `ui/src/App.tsx`: about 7,977 lines
- `ui/src/App.test.tsx`: about 15,271 lines

Success means:
- `App.tsx` becomes a composition root instead of a state/effect/render dump.
- `App.test.tsx` is replaced by a set of domain test files with shared test harness utilities.
- Every slice lands with green verification before the next slice starts.

## Non-goals

- No feature work.
- No visual redesign.
- No broad naming cleanup unrelated to the split.
- No architecture rewrite to context/reducer/state machine unless a slice cannot be completed cleanly without it.
- No "improve while here" refactors.

## Operating Model

There are two roles:

1. Implementation agent
   - Performs a single planned slice.
   - Stops after that slice.
   - Does not mix multiple domains into one change unless the plan explicitly says so.

2. Verification agent
   - Reviews the diff against this plan.
   - Runs the required commands.
   - Rejects the slice if behavior, ownership, or test scope drifted.

This split only works if verification is stricter than implementation.

## Rules

1. One slice per commit.
2. Every slice must preserve behavior.
3. Move tests before removing old coverage.
4. Keep file ownership obvious.
5. Prefer extraction over abstraction.
6. Keep imports direct; do not keep growing `App.tsx` re-exports.
7. If a slice needs a second unrelated cleanup to compile, that is a sign the slice is too large.

## Target End State

### Production files

Create or fill these modules:

- `ui/src/app-test-harness.tsx`
- `ui/src/app-live-state.ts`
- `ui/src/app-workspace-layout.ts`
- `ui/src/app-session-actions.ts`
- `ui/src/app-preferences-state.ts`
- `ui/src/app-drag-resize.ts`
- `ui/src/AppControlSurface.tsx`
- `ui/src/AppDialogs.tsx`

Keep `ui/src/App.tsx` as:
- app bootstrap
- top-level state wiring
- high-level derived state
- hook composition
- final JSX shell assembly

### Test files

Replace `ui/src/App.test.tsx` with domain files:

- `ui/src/MarkdownContent.test.tsx`
- `ui/src/App.live-state.test.tsx`
- `ui/src/App.workspace-layout.test.tsx`
- `ui/src/App.session-lifecycle.test.tsx`
- `ui/src/App.orchestrators.test.tsx`
- `ui/src/App.control-panel.test.tsx`
- `ui/src/App.control-panel-dnd.test.tsx`
- `ui/src/App.scroll-behavior.test.tsx`
- optionally `ui/src/App.smoke.test.tsx`

Final target:
- `App.tsx` should be under about 2,000 lines.
- No single new test file should exceed about 2,500 lines.

## High-Risk Areas

These must be treated as behavioral boundaries:

- SSE state adoption, reconnect, watchdog, and action-recovery flows
- workspace layout persistence and restart recovery
- session creation, fetch hydration, and stale-response gates
- control-panel pane-local session/project scoping
- drag/drop and split-pane resize behavior
- message scroll restoration and virtualized session behavior

Any slice touching one of these domains must carry focused verification.

## Before Any Split

### Slice 0: Baseline and decoupling

Implementation tasks:
- Stop importing non-App helpers through `App.tsx` where possible.
- Move test imports to their real modules:
  - `MarkdownContent` from `message-cards`
  - `ThemedCombobox` from `preferences-panels`
  - session model utility functions from `session-model-utils`
- Keep `App.tsx` re-exports only until all consumers are moved.

Acceptance criteria:
- No behavior changes.
- `App.tsx` import/export surface shrinks, not grows.

Verification:
- `cd ui && npx tsc --noEmit`
- `cd ui && npx vitest run src/App.test.tsx src/MarkdownContent.test.tsx`
- `cd ui && npm run build`

## Phase 1: Extract Shared App Test Harness

### Slice 1: Shared harness file

Create `ui/src/app-test-harness.tsx`.

Move from `App.test.tsx`:
- `EventSourceMock`
- `ResizeObserverMock`
- `jsonResponse`
- `restoreGlobal`
- `flushUiWork`
- `settleAsyncUi`
- `advanceTimers`
- `renderApp`
- `renderAppWithProjectAndSession`
- `makeStateResponse`
- `makeSession`
- scroll mocks and geometry helpers
- `withSuppressedActWarnings`
- any fixture builders used across multiple new files

Rules:
- No test logic changes.
- Keep helper names stable unless there is a collision.
- Do not move one-off local helpers into the harness.

Acceptance criteria:
- `App.test.tsx` still passes unchanged after importing from the new harness.
- Harness contains reusable setup only.

Verification:
- `cd ui && npx vitest run src/App.test.tsx`
- `cd ui && npx tsc --noEmit`

## Phase 2: Split `App.test.tsx`

Do these slices in order.

### Slice 2: Move MarkdownContent tests out

Move the block currently starting around `describe("MarkdownContent")` into:
- `ui/src/MarkdownContent.test.tsx`

Acceptance criteria:
- `App.test.tsx` no longer contains MarkdownContent tests.
- The new file imports the real modules, not `App.tsx`.

Verification:
- `cd ui && npx vitest run src/MarkdownContent.test.tsx`
- `cd ui && npx tsc --noEmit`

### Slice 3: Live state / reconnect / watchdog tests

Move the large SSE/reconnect/watchdog cluster into:
- `ui/src/App.live-state.test.tsx`

Scope includes:
- reconnect snapshot adoption
- fallback `/api/state` resync
- stale transport watchdog
- wake-gap recovery
- delta-gap handling
- session hydration resyncs

Acceptance criteria:
- No workspace or control-panel tests moved in this slice.
- New file owns all live-state transport scenarios.

Verification:
- `cd ui && npx vitest run src/App.live-state.test.tsx`
- `cd ui && npx tsc --noEmit`

### Slice 4: Workspace layout and workspace switcher tests

Move into:
- `ui/src/App.workspace-layout.test.tsx`

Scope includes:
- workspace switcher open/load/delete
- workspace layout load/save
- keepalive flush on `pagehide`
- workspace restart recovery notices

Verification:
- `cd ui && npx vitest run src/App.workspace-layout.test.tsx`
- `cd ui && npx tsc --noEmit`

### Slice 5: Session lifecycle tests

Move into:
- `ui/src/App.session-lifecycle.test.tsx`

Scope includes:
- create session
- model refresh
- unknown-model send confirmation
- backend-unavailable create-session recovery
- fetched session hydration and stale gates

Verification:
- `cd ui && npx vitest run src/App.session-lifecycle.test.tsx`
- `cd ui && npx tsc --noEmit`

### Slice 6: Orchestrator tests

Move into:
- `ui/src/App.orchestrators.test.tsx`

Scope includes:
- orchestrator delta adoption
- grouped session rendering
- runtime actions and action errors

Verification:
- `cd ui && npx vitest run src/App.orchestrators.test.tsx`
- `cd ui && npx tsc --noEmit`

### Slice 7: Control panel and pane-local scoping tests

Move into:
- `ui/src/App.control-panel.test.tsx`

Scope includes:
- project scope combobox behavior
- standalone tabs
- pane-local session/project scoping
- Files/Git/Canvas opening behavior

Verification:
- `cd ui && npx vitest run src/App.control-panel.test.tsx`
- `cd ui && npx tsc --noEmit`

### Slice 8: Control panel drag/drop tests

Move into:
- `ui/src/App.control-panel-dnd.test.tsx`

Scope includes:
- dock drag/drop
- body dragover MIME fallback
- tab rail drops

Verification:
- `cd ui && npx vitest run src/App.control-panel-dnd.test.tsx`
- `cd ui && npx tsc --noEmit`

### Slice 9: Scroll, layout clamp, and keyboard-scroll tests

Move into:
- `ui/src/App.scroll-behavior.test.tsx`

Scope includes:
- session scroll restoration
- wheel passive/non-passive behavior
- control-panel width clamps
- jump-to-top / bottom scroll regressions

Verification:
- `cd ui && npx vitest run src/App.scroll-behavior.test.tsx`
- `cd ui && npx tsc --noEmit`

### Slice 10: Final App smoke file

Leave a very small final file:
- `ui/src/App.smoke.test.tsx`

Suggested contents:
- app renders
- a single control-panel smoke interaction
- a single session-open smoke interaction

At this point:
- remove or delete `ui/src/App.test.tsx`

Verification:
- `cd ui && npm test`
- `cd ui && npm run build`

## Phase 3: Split `App.tsx`

Do not start this phase until Phase 2 is stable.

### Slice 11: Preferences state and persistence

Extract to:
- `ui/src/app-preferences-state.ts`

Move:
- theme/style/markdown/diagram state
- preference persistence effects
- default agent/session preference state

Keep in `App.tsx`:
- only the values/callbacks needed to render settings UI

Acceptance criteria:
- No settings dialog JSX moves yet.
- This slice is state/effects only.

Verification:
- `cd ui && npx vitest run src/App.session-lifecycle.test.tsx src/App.workspace-layout.test.tsx`
- `cd ui && npx tsc --noEmit`
- `cd ui && npm run build`

### Slice 12: Workspace layout module

Extract to:
- `ui/src/app-workspace-layout.ts`

Move:
- workspace summary request token logic
- workspace switcher refresh/delete handlers
- pending layout save logic
- load/save/keepalive behavior
- workspace restart recovery helpers

Acceptance criteria:
- New module owns workspace persistence lifecycle.
- `App.tsx` calls a hook/helper and consumes returned state/actions.

Verification:
- `cd ui && npx vitest run src/App.workspace-layout.test.tsx`
- `cd ui && npx tsc --noEmit`
- `cd ui && npm run build`

### Slice 13: Live state transport module

Extract to:
- `ui/src/app-live-state.ts`

Move:
- initial `/api/state` bootstrap
- SSE `state` event handling
- SSE `delta` event handling
- workspace-files-changed event handling
- reconnect fallback timers
- watchdog timers
- visibility/focus/pagehide/pageshow handlers
- session hydration fetch effect

Acceptance criteria:
- This is the biggest and highest-risk slice.
- Keep the current logic shape; do not redesign transport behavior.
- Preserve ref-based gates and same-tick ordering.

Verification:
- `cd ui && npx vitest run src/App.live-state.test.tsx src/App.session-lifecycle.test.tsx`
- `cd ui && npx tsc --noEmit`
- `cd ui && npm run build`

### Slice 14: Session action module

Extract to:
- `ui/src/app-session-actions.ts`

Move:
- create session/project
- send/stop/kill/rename session
- update session settings
- refresh model options
- fetch agent commands
- approval/user-input/MCP/app-request submit handlers

Acceptance criteria:
- Action functions keep the same response adoption rules.
- No UI render helpers move in this slice.

Verification:
- `cd ui && npx vitest run src/App.session-lifecycle.test.tsx src/App.orchestrators.test.tsx`
- `cd ui && npx tsc --noEmit`
- `cd ui && npm run build`

### Slice 15: Control surface render extraction

Extract the large nested render block around the current control-surface section into:
- `ui/src/AppControlSurface.tsx`

Move:
- `renderWorkspaceControlSurface`
- session row rendering
- control-panel project scope UI
- header actions
- section rendering for sessions/projects/files/git/orchestrators

Rules:
- This component should be render-focused.
- Do not silently re-own state that still belongs in `App.tsx`.
- Pass data and callbacks explicitly.

Acceptance criteria:
- No nested 1,000-line render helper remains inside `App.tsx`.

Verification:
- `cd ui && npx vitest run src/App.control-panel.test.tsx src/App.control-panel-dnd.test.tsx src/App.orchestrators.test.tsx`
- `cd ui && npx tsc --noEmit`
- `cd ui && npm run build`

### Slice 16: Dialog extraction

Extract to:
- `ui/src/AppDialogs.tsx`

Move:
- create session dialog JSX
- create project dialog JSX
- settings dialog JSX
- rename and kill popovers if cleanly separable

Acceptance criteria:
- Dialog state may stay in `App.tsx` if needed.
- The extraction should mostly remove JSX and dialog-specific wiring noise.

Verification:
- `cd ui && npx vitest run src/App.session-lifecycle.test.tsx src/App.control-panel.test.tsx`
- `cd ui && npx tsc --noEmit`
- `cd ui && npm run build`

### Slice 17: Drag/resize extraction

Extract to:
- `ui/src/app-drag-resize.ts`

Move:
- dragged tab state
- launcher/external drag state
- resize state ref
- drag channel lifecycle

Acceptance criteria:
- DnD semantics unchanged.
- Broadcast-channel behavior unchanged.

Verification:
- `cd ui && npx vitest run src/App.control-panel-dnd.test.tsx`
- `cd ui && npx tsc --noEmit`
- `cd ui && npm run build`

### Slice 18: Final App cleanup

End state for `App.tsx`:
- shell state wiring
- imported hook usage
- small derived selectors
- top-level layout render

Remove leftover `App.tsx` re-exports that are no longer needed.

Verification:
- `cd ui && npm test`
- `cd ui && npm run build`

## Verification Standard

For every slice, the verification agent must check:

1. Scope control
   - Only the intended domain moved.
   - No unrelated behavior changes.

2. Ownership clarity
   - New module has a coherent responsibility.
   - `App.tsx` shrank in real logic, not just line wrapping.

3. Test quality
   - Moved tests still assert behavior, not implementation details.
   - Helpers in `app-test-harness.tsx` are generic and reused.

4. Build integrity
   - Typecheck passes.
   - Production build passes.

5. Regression safety
   - Domain tests plus neighboring-risk tests pass.

## Stop Conditions

Stop the implementation agent immediately if:
- a slice needs changes in two unrelated domains
- a slice introduces `AppContext` just to avoid prop threading
- a moved hook starts owning render-only concerns
- verification shows changed behavior rather than pure relocation
- the new file is still over about 3,000 lines after the move

## Final Done Criteria

The split is complete when:
- `App.tsx` is no longer the primary home for transport, persistence, actions, and giant nested render helpers
- `App.test.tsx` no longer exists as a monolith
- every new domain file has clear ownership
- all frontend tests pass
- `npm run build` passes

## Recommended First Three Slices

If a fresh implementation agent starts from this plan, the right opening sequence is:

1. Slice 1: create `app-test-harness.tsx`
2. Slice 2: move `MarkdownContent` tests out
3. Slice 3: split `App.live-state.test.tsx`

That sequence reduces risk fastest and gives verification the most leverage before production-code extraction starts.
