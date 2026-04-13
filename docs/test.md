# Test Plan

This document describes the current TermAl test strategy and the commands used
before reviews or releases.

## Current State

TermAl has both backend and frontend automated tests.

Backend:

- Rust unit and route tests live in `src/tests.rs`.
- Tests cover state persistence, remote routing, SSE parsing, terminal command
  execution, project deletion, workspace layouts, file/git APIs, agent-runtime
  normalization, and regression edges.
- Run with `cargo test`.

Frontend:

- Vitest + React Testing Library are configured in the Vite app.
- Test files live next to the TypeScript modules they exercise, using
  `*.test.ts` and `*.test.tsx`.
- Coverage includes workspace state, tab drag/drop, live updates, session
  reconciliation, model options, slash-command behavior, message cards,
  source/diff/file/git panels, terminal panel behavior, remotes, themes, and
  the main `App` integration harness.
- Run with `cd ui && npx vitest run`.

Type/build checks:

- Backend compile check: `cargo check`.
- Frontend type check: `cd ui && npx tsc --noEmit`.
- Frontend production build: `cd ui && npm run build`.

## Review Gate

Before a code review of staged/unstaged work:

```bash
cargo check
cd ui && npx tsc --noEmit
```

If either command reports errors, stop and fix those first. Warnings can be
reported and triaged with the review.

For higher-confidence changes, also run:

```bash
cargo test
cd ui && npx vitest run
```

## Backend Testing Guidelines

Prefer focused Rust tests in `src/tests.rs` for:

- pure parsers and normalizers
- path validation and canonicalization
- persistence projections
- API route behavior through `tower::ServiceExt`
- remote proxy edge cases
- terminal stream framing, cancellation, truncation, and 429 behavior

Backend tests should avoid starting real agents. Use test HTTP listeners,
temporary directories, injected remote configs, and helper state builders
instead.

## Frontend Testing Guidelines

Prefer pure TypeScript tests for reducers and helpers:

- workspace tree/tab operations
- path display and validation
- state revision adoption
- live delta application
- session model option normalization
- remote config normalization

Use React Testing Library when the regression depends on rendered behavior:

- keyboard navigation
- combobox selection
- focus management
- scroll behavior
- panel-specific user flows
- stale file and conflict recovery actions
- terminal streaming UI

Keep integration tests focused. The main `App.test.tsx` harness is valuable but
expensive; prefer extracting pure helpers or testing a panel directly when that
captures the bug.

## Known Coverage Gaps

The active follow-up list lives in `docs/bugs.md` under **Implementation Tasks**.
Those tasks are not active bugs; they are P2 coverage or type-surface
improvements.

Current gaps:

- multi-commit session scroll pinning needs a render-level regression test
- one settled-scroll integration test should assert the explicit `minAttempts`
  floor more directly
- the exported `isScrollContainerAtBottom` helper needs either deletion or a
  comment explaining why the dead export is intentional
- `setAppTestHooksForTests` should either be tree-shaken from production or
  documented as non-sensitive test-only surface
- `AppTestHooks` should be exported for cleaner test typing
- `resolveSettledScrollMinimumAttempts(0)` needs a small edge assertion

## What Not To Test

- Do not test full external agent CLIs in normal unit suites.
- Do not depend on a user's real `~/.termal` directory.
- Do not require network access for default tests.
- Do not make frontend tests depend on real Monaco layout measurements unless
  the test is explicitly about Monaco integration.
- Do not add broad snapshots of the whole app; assert the behavioral contract
  being protected.
