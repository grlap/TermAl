# App Split Plan

## Goal

Split the oversized frontend entrypoint and its original monolithic integration
test file into smaller, reviewable modules without changing behavior.

Success means:
- `ui/src/App.tsx` becomes a composition root instead of a state/effect/render dump.
- The old `ui/src/App.test.tsx` monolith stays replaced by domain suites plus a
  shared test harness.
- Every remaining slice lands with green verification before the next slice starts.

## Current Audit (2026-04-21)

This plan has been re-audited against the current tree after a partial split and
several unrelated follow-up fixes. The status below reflects what is actually in
the repo now, not what the original plan expected to happen next.

### Current metrics

- `ui/src/App.tsx`: about 1,992 lines
- `ui/src/App.test.tsx`: removed
- `ui/src/app-test-harness.tsx`: about 651 lines
- `ui/src/app-preferences-state.ts`: about 199 lines
- `ui/src/app-workspace-layout.ts`: about 603 lines
- `ui/src/app-live-state.ts`: about 1,609 lines
- `ui/src/app-session-actions.ts`: about 1,417 lines
- `ui/src/app-dialog-state.ts`: about 604 lines
- `ui/src/app-drag-resize.ts`: extracted
- `ui/src/app-workspace-actions.ts`: extracted
- `ui/src/app-control-panel-state.ts`: extracted
- `ui/src/AppControlSurface.tsx`: extracted
- `ui/src/AppDialogs.tsx`: extracted
- `ui/src/App.smoke.test.tsx`: about 415 lines
- `ui/src/App.live-state.deltas.test.tsx`: about 1,270 lines
- `ui/src/App.live-state.visibility.test.tsx`: about 1,164 lines
- `ui/src/App.live-state.watchdog.test.tsx`: about 2,079 lines
- `ui/src/App.control-panel.test.tsx`: about 567 lines
- `ui/src/App.control-panel.scoping.test.tsx`: about 1,685 lines
- `ui/src/App.control-panel.openers.test.tsx`: about 1,265 lines

### Completed work

The following original slices are complete:

- **Slice 0 complete**: `App.tsx` no longer re-exports helper symbols; it only
  exports the default `App` component.
- **Slice 1 complete**: shared harness lives in
  `ui/src/app-test-harness.tsx`.
- **Phase 2 functionally complete**: the old `App.test.tsx` monolith is gone.
  The current domain test set is:
  - `ui/src/MarkdownContent.test.tsx`
  - `ui/src/App.live-state.deltas.test.tsx`
  - `ui/src/App.live-state.reconnect.test.tsx`
  - `ui/src/App.live-state.visibility.test.tsx`
  - `ui/src/App.live-state.watchdog.test.tsx`
  - `ui/src/App.workspace-layout.test.tsx`
  - `ui/src/App.session-lifecycle.test.tsx`
  - `ui/src/App.orchestrators.test.tsx`
  - `ui/src/App.control-panel.test.tsx`
  - `ui/src/App.control-panel.scoping.test.tsx`
  - `ui/src/App.control-panel.openers.test.tsx`
  - `ui/src/App.control-panel-dnd.test.tsx`
  - `ui/src/App.scroll-behavior.test.tsx`
  - `ui/src/App.preferences.test.tsx`
  - `ui/src/App.smoke.test.tsx`
- **Slice 3R complete**: the oversized live-state delta/watchdog suite is now
  split across:
  - `ui/src/App.live-state.deltas.test.tsx`
  - `ui/src/App.live-state.visibility.test.tsx`
  - `ui/src/App.live-state.watchdog.test.tsx`
- **Slice 7R complete**: the oversized control-panel suite is now split across:
  - `ui/src/App.control-panel.test.tsx`
  - `ui/src/App.control-panel.scoping.test.tsx`
  - `ui/src/App.control-panel.openers.test.tsx`
- **Slice 11 complete**: preference state/persistence moved to
  `ui/src/app-preferences-state.ts` via `useAppPreferencesState`.
- **Slice 12 complete**: workspace layout/state persistence moved to
  `ui/src/app-workspace-layout.ts` via `useAppWorkspaceLayout`.
- **Slices 13A and 13B complete**: live-state transport, hydration, reconnect,
  watchdog, and browser lifecycle recovery moved to
  `ui/src/app-live-state.ts` via `useAppLiveState`.
- **Slice 14 complete**: session/project action orchestration moved to
  `ui/src/app-session-actions.ts` via `useAppSessionActions`.
- **Slice 15 complete**: control-surface rendering moved to
  `ui/src/AppControlSurface.tsx`.
- **Slice 16 complete**: dialog and popover JSX moved to
  `ui/src/AppDialogs.tsx`, with dialog state/orchestration moved to
  `ui/src/app-dialog-state.ts`.
- **Slice 17 complete**: drag/resize state and channel lifecycle moved to
  `ui/src/app-drag-resize.ts` via `useAppDragResize`.
- **Final cleanup substantially complete**:
  - pane/workspace action orchestration moved to
    `ui/src/app-workspace-actions.ts`
  - control-panel and create-session derived state moved to
    `ui/src/app-control-panel-state.ts`
  - `ui/src/App.tsx` is now under 2,000 lines and acts as the composition root
    for the extracted hooks/components

### What is still open

The split is functionally complete.

Remaining follow-up is optional and narrow:

- if the stricter stretch target still matters, extract the remaining workspace
  shell render assembly from `ui/src/App.tsx` to push it below ~1,500 lines
- otherwise, keep the current shape and treat future reductions as opportunistic

The test split is complete and compliant with the current size targets.

### Out-of-scope changes since the partial split

Several unrelated fixes landed after the split started, including:

- virtualization / scrolling fixes
- session transcript rendering fixes
- docs and bug-tracking updates
- backend/runtime changes

These do **not** change split status. Do not treat them as split progress unless
they directly move responsibility out of `App.tsx` or further break down the App
test surface.

## Rules

1. One slice per commit.
2. Every slice must preserve behavior.
3. Move tests before removing coverage.
4. Keep file ownership obvious.
5. Prefer extraction over abstraction.
6. Keep imports direct; do not reintroduce `App.tsx` helper re-exports.
7. If a slice needs changes in two unrelated domains to compile, the slice is
   too large.
8. After each slice, stop and wait for human approval before starting the next
   slice.

## Commit Convention

Use this prefix for every split commit:

- `App-split/slice-N: <summary>`

Examples:
- `App-split/slice-14: extract app session actions hook`
- `App-split/slice-15: extract AppControlSurface component`

This is required so the split stays traceable in `git log --oneline`.

## Extraction Shape Convention

Use these shapes consistently:

- If the extracted module owns React state, refs, effects, or callback
  orchestration, it should be a custom hook.
  - Examples: `useAppPreferencesState`, `useAppWorkspaceLayout`,
    `useAppLiveState`, `useAppSessionActions`, `useAppDragResize`
- If the extracted module is render-heavy JSX with callbacks and derived props
  passed in, it should be a component.
  - Examples: `AppControlSurface`, `AppDialogs`
- If the extracted module is pure logic, keep it as plain functions.

Do not create large helper functions that simulate hooks by taking dozens of
mutable refs and setters as arguments.

## Target End State

### Production files

Completed:
- `ui/src/app-test-harness.tsx`
- `ui/src/app-preferences-state.ts`
- `ui/src/app-workspace-layout.ts`
- `ui/src/app-live-state.ts`
- `ui/src/app-session-actions.ts`
- `ui/src/app-dialog-state.ts`
- `ui/src/app-drag-resize.ts`
- `ui/src/app-workspace-actions.ts`
- `ui/src/app-control-panel-state.ts`
- `ui/src/AppControlSurface.tsx`
- `ui/src/AppDialogs.tsx`

### Test files

Current final-state test set should be:
- `ui/src/MarkdownContent.test.tsx`
- `ui/src/App.live-state.deltas.test.tsx`
- `ui/src/App.live-state.reconnect.test.tsx`
- `ui/src/App.live-state.visibility.test.tsx`
- `ui/src/App.live-state.watchdog.test.tsx`
- `ui/src/App.workspace-layout.test.tsx`
- `ui/src/App.session-lifecycle.test.tsx`
- `ui/src/App.orchestrators.test.tsx`
- `ui/src/App.control-panel.test.tsx`
- `ui/src/App.control-panel.scoping.test.tsx`
- `ui/src/App.control-panel.openers.test.tsx`
- `ui/src/App.control-panel-dnd.test.tsx`
- `ui/src/App.scroll-behavior.test.tsx`
- `ui/src/App.preferences.test.tsx`
- `ui/src/App.smoke.test.tsx`

Final targets:
- `ui/src/App.tsx` under about 2,000 lines, with under about 1,500 as the
  remaining stretch target if more reduction is worth the churn
- no single domain test file above about 2,500 lines

## Verification Standard

For any future follow-up slice, the verification agent must check:

1. Scope control
   - Only the intended domain moved.
   - No unrelated behavior changes.

2. Ownership clarity
   - New module has a coherent responsibility.
   - `App.tsx` shrank in real logic, not just line wrapping.

3. Test quality
   - Moved tests still assert behavior, not implementation details.
   - Helpers in `app-test-harness.tsx` remain generic and reused.

4. Build integrity
   - Typecheck passes.
   - Production build passes.

5. Regression safety
   - Domain tests plus neighboring-risk tests pass.

6. Backend boundary
   - Backend test suite is not expected to change for frontend-only slices.
   - If a slice touches shared files used by backend-facing tests, the verifier
     must explicitly call that out before asking for backend verification.

## Stop Conditions

Stop the implementation agent immediately if:
- a slice needs changes in two unrelated domains
- a slice introduces `AppContext` just to avoid prop threading
- a moved hook starts owning render-only concerns
- verification shows changed behavior rather than pure relocation
- the new file is still over about 3,000 lines after the move

## Abort Protocol

If a slice aborts:

1. Revert the in-progress slice commit or discard the uncommitted worktree changes.
2. Document what blocked the slice in this plan or in the handoff note.
3. Split the blocked slice into two smaller slices before retrying.
4. Do not proceed to the next slice.
5. Get human approval on the revised smaller plan before restarting implementation.

## Final Done Criteria

The split is complete when:
- `App.tsx` is no longer the primary home for transport, persistence, actions,
  dialogs, drag/resize, and giant nested render helpers
- the oversized domain tests are split down to the size target
- all frontend tests pass
- `npm run build` passes
