# React & TypeScript Review

Focus: Hook correctness, component performance, type safety, React 18 patterns.
## Development-Phase Compatibility Policy
- Legacy compatibility means supporting older persisted schema or older local/internal API shapes from previous development builds, such as obsolete orchestrator fields.
- Do NOT flag missing schema upgrades, migrations, or backward compatibility for ~/.termal/*.json, browser localStorage state, or local/internal API contracts from previous local-only development builds.
- Path normalization and canonicalization for current inputs are not legacy compatibility work.
- Intentional breaking changes are acceptable during development; only flag compatibility issues when they break current-tree behavior, current tests, or the current documented contract.
- Windows, macOS, and Linux are P0 platforms. Flag regressions on those platforms; do not require support beyond them unless the current change claims it.

## What to check

1. **Hook dependency arrays**: Every `useEffect`, `useCallback`, `useMemo` must have correct deps:
   - Flag missing dependencies that could cause stale closures
   - Flag unnecessary dependencies that cause excessive re-renders
   - Flag `// eslint-disable-next-line` on dependency warnings without explanation
   - Pay special attention to refs (`useRef`) — ref objects are stable but `.current` is not reactive

2. **State updates after unmount**: Async operations must guard against unmounted components:
   - Flag `setState` calls after `await` without checking if the component is still mounted
   - Flag missing cleanup in `useEffect` return for subscriptions or timers
   - SSE `EventSource` must be closed in effect cleanup

3. **Event handler identity**: Callbacks passed to child components or event listeners:
   - Flag inline arrow functions in JSX that cause unnecessary re-renders of memoized children
   - Flag `useCallback` with frequently-changing deps that defeat memoization
   - Exception: simple event handlers on leaf DOM elements are fine inline

4. **Type safety**:
   - Flag `any` type usage — prefer specific types or `unknown` with type guards
   - Flag type assertions (`as Type`) that bypass type checking without runtime validation
   - Flag optional chaining (`?.`) used to paper over missing null checks in types
   - Ensure `types.ts` types match the backend's JSON response shapes exactly

5. **SSE / real-time handling**:
   - Flag revision ordering violations (accepting stale state, missing gap detection)
   - Flag delta application that mutates state directly instead of producing new objects
   - `reconcileSessions()` must preserve object identity for unchanged sessions
   - Flag missing error handling on `EventSource` connection failures

6. **Workspace state management**:
   - Binary tree operations (split, close, move tab) must maintain tree invariants
   - Flag orphaned panes or tabs after split/close operations
   - Flag workspace state updates that don't trigger proper re-renders
   - Tab drag-drop must handle edge cases (same pane, last tab, cross-pane)

7. **Monaco Editor integration**:
   - Flag Monaco instances not properly disposed on unmount
   - Flag theme mismatches between TermAl themes and Monaco themes
   - Flag language detection that doesn't handle edge cases (unknown extensions)

8. **Accessibility basics**:
   - Flag interactive elements without keyboard support (click handlers without onKeyDown)
   - Flag missing `role` attributes on custom widgets (slash menu, approval cards)
   - Flag missing `aria-label` on icon-only buttons

## What NOT to flag

- Single-file architecture (`App.tsx`) — known tradeoff for iteration speed
- `useState` over external state libraries — intentional design choice
- CSS-in-JS vs CSS files — project uses plain CSS with variables
- Component file organization — current structure is documented and intentional

