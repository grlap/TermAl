# Testing Review

Focus: Test coverage for changes, test quality, Vitest patterns.
## Development-Phase Compatibility Policy
- Legacy compatibility means supporting older persisted schema or older local/internal API shapes from previous development builds, such as obsolete orchestrator fields.
- Do NOT flag missing schema upgrades, migrations, or backward compatibility for ~/.termal/*.json, browser localStorage state, or local/internal API contracts from previous local-only development builds.
- Path normalization and canonicalization for current inputs are not legacy compatibility work.
- Intentional breaking changes are acceptable during development; only flag compatibility issues when they break current-tree behavior, current tests, or the current documented contract.
- Windows, macOS, and Linux are P0 platforms. Flag regressions on those platforms; do not require support beyond them unless the current change claims it.

## What to check

1. **New functionality coverage**: Changes that add new behavior should have tests:
   - New utility functions (workspace.ts, live-updates.ts, diff-preview.ts, etc.) need unit tests
   - New API response parsing logic needs tests with sample JSON
   - New state reconciliation paths need tests
   - Flag behavioral additions with no corresponding test

2. **Changed behavior**: If existing behavior changed, are tests updated?
   - Tests that still pass but test the old behavior = false confidence
   - Flag behavioral changes with no test updates
   - Flag modified functions whose existing tests don't cover the new code path

3. **Test quality**:
   - Tests assert something meaningful (not just "doesn't throw")
   - Test names describe the scenario (`it("preserves object identity when session unchanged")`)
   - No flaky tests (time-dependent, order-dependent)
   - Tests are independent (don't rely on shared mutable state between tests)
   - Flag `expect(result).toBeTruthy()` when a specific value check is possible

4. **React component tests** (Vitest + React Testing Library):
   - Test user interactions, not implementation details
   - Flag tests that reach into component internals (checking state, calling hooks directly)
   - Flag missing `cleanup` or `unmount` after render
   - Flag `waitFor` without proper assertions inside
   - Use `screen.getByRole`, `getByText` over `getByTestId` where possible

5. **Edge cases for state management**:
   - Empty session list
   - Single pane workspace (can't split further or close last pane)
   - Stale revision handling (delta with gap, state with lower revision)
   - Rapid successive state updates
   - SSE reconnection scenarios

6. **Snapshot / diff parsing tests**:
   - Unified diff parsing edge cases (empty diff, binary files, new files, deleted files)
   - Monaco diff reconstruction accuracy
   - Syntax highlighting for unusual file types

7. **Backend test considerations** (if Rust tests are added):
   - Session lifecycle state machine transitions
   - Concurrent state access patterns
   - JSON serialization round-trip correctness
   - Agent protocol message parsing

## What NOT to flag

- Missing end-to-end tests (not set up yet)
- Missing Rust unit tests (current focus is frontend tests)
- Low overall test coverage percentage (known — project is growing tests incrementally)
- Test file organization (tests co-located with source is the current pattern)

