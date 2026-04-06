# Architecture Review

Focus: State management, layer boundaries, agent-agnostic patterns, design consistency.
## Development-Phase Compatibility Policy
- Legacy compatibility means supporting older persisted schema or older local/internal API shapes from previous development builds, such as obsolete orchestrator fields.
- Do NOT flag missing schema upgrades, migrations, or backward compatibility for ~/.termal/*.json, browser localStorage state, or local/internal API contracts from previous local-only development builds.
- Path normalization and canonicalization for current inputs are not legacy compatibility work.
- Intentional breaking changes are acceptable during development; only flag compatibility issues when they break current-tree behavior, current tests, or the current documented contract.
- Windows, macOS, and Linux are P0 platforms. Flag regressions on those platforms; do not require support beyond them unless the current change claims it.

## What to check

1. **State mutation pattern**: All client-visible state changes must go through `commit_locked()`:
   - Flag direct field mutations on `StateInner` that skip `commit_locked()`
   - Flag missing revision bumps for changes the frontend needs to see
   - Exception: internal bookkeeping (e.g., recording runtime config) may use `persist_state()` directly
   - Delta events (`DeltaEvent`) must be used for streaming paths — flag full state publishes in hot loops

2. **Agent-agnostic message model**: Both Claude and Codex (and future agents) produce the same `Message` variants:
   - Flag agent-specific logic in frontend rendering code (the frontend should not know which agent produced a message)
   - Flag new message types that only work for one agent
   - New agents should produce standard `Message` variants (Text, Command, Diff, Approval, Markdown, Thinking)

3. **Session lifecycle correctness**: Status transitions must follow the documented lifecycle:
   ```
   Idle → Active (on send_message)
   Active → Approval (on tool approval request)
   Approval → Active (on approval decision)
   Active → Idle (on turn complete)
   Active/Approval → Idle (on stop)
   ```
   - Flag status transitions that skip states or leave orphaned runtime handles
   - Flag missing `dispatch_next_queued_turn()` calls after status → Idle transitions

4. **Concurrency safety**: `AppState` uses `Arc<Mutex<StateInner>>`:
   - Flag holding the mutex lock across `.await` points (this will deadlock tokio)
   - Flag `.unwrap()` on mutex lock without context (use `.expect("reason")`)
   - Flag missing lock acquisitions when reading/writing shared state

5. **SSE protocol consistency**: Events must carry correct revision numbers:
   - Flag delta events that don't bump revision
   - Flag full state events sent where a delta would suffice (performance)
   - Flag missing revision fields on new event types

6. **Separation of concerns**:
   - Backend (`main.rs`) handles state, persistence, agent protocols
   - Frontend (`App.tsx`) handles rendering, workspace layout, user interaction
   - Flag business logic leaking into the wrong layer (e.g., agent protocol parsing in frontend)
   - Flag UI concerns leaking into backend (e.g., formatting, display logic)

7. **Persistence alignment**: `PersistedState` must include all fields needed for session restore:
   - Flag new session fields that aren't persisted but should survive a restart
   - Flag persisted fields that include runtime-only data (handles, channels)

## What NOT to flag

- The fact that `main.rs` is a single large file (this is a known intentional tradeoff)
- The fact that `App.tsx` is a single large file (same reason — iteration speed)
- Performance micro-optimizations unless architecturally significant
- Code style preferences (formatting, naming) — leave to linters

