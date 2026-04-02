# Bugs & Known Issues

Updated against the current checked-in code in `src/main.rs`, `ui/src/App.tsx`,
`ui/package.json`, and `ui/vite.config.ts`.

Completed items are removed from this file once fixed. The sections below track
active bugs and follow-up tasks only.

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

