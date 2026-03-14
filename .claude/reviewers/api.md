# API & Protocol Review

Focus: REST endpoint design, SSE protocol, agent communication protocols, consistency.

## What to check

1. **REST API consistency**:
   - Flag inconsistent HTTP methods (e.g., GET for mutations, POST for reads)
   - Flag inconsistent response shapes (some return `StateResponse`, some return custom types)
   - Flag missing error responses (all endpoints should return `ApiError` JSON on failure)
   - Flag new endpoints not documented in `docs/architecture.md`
   - Status codes: 200 for reads, 201 for creation, 202 for async acceptance, 400/404/500 for errors

2. **Request/response contract**:
   - `camelCase` for JSON keys (serde `rename_all = "camelCase"`)
   - Flag snake_case keys in JSON responses
   - Flag missing fields in response types that the frontend expects
   - Flag frontend `types.ts` types that don't match backend response structs

3. **SSE protocol correctness**:
   - `state` events must carry full `StateResponse` with current revision
   - `delta` events must carry `DeltaEvent` with the exact next revision number
   - Flag new event types that don't follow the `state`/`delta` convention
   - Flag events that could exceed reasonable size limits (full state with many long sessions)

4. **Claude protocol (NDJSON over stdio)**:
   - Messages to Claude: `control_request` (initialize), `user` (prompt), `control_response` (approval)
   - Messages from Claude: `assistant` (streaming), `result` (turn complete), `control_request` (approval needed)
   - Flag protocol messages with incorrect structure or missing required fields
   - Flag missing handling for new Claude event types

5. **Codex protocol (JSON-RPC 2.0 over stdio)**:
   - RPCs: `initialize`, `thread/start`, `thread/resume`, `turn/start`
   - Notifications from Codex: `item/*/delta`, `item/started`, `item/completed`, `turn/completed`
   - Flag JSON-RPC messages missing `jsonrpc: "2.0"` or `id` fields
   - Flag missing notification handlers for new Codex event types

6. **Error propagation**:
   - Backend errors should be `ApiError` with status code and message
   - Frontend should handle all error responses gracefully (show to user, not crash)
   - Flag agent protocol errors that crash the session instead of setting status to Error
   - Flag network errors not caught in API client (`api.ts`)

7. **Queueing and ordering**:
   - Queued prompts must be dispatched in FIFO order
   - Flag race conditions between concurrent API calls to the same session
   - Flag missing queue cleanup on session kill/stop

## What NOT to flag

- The fact that both Claude and Codex use different protocols (this is by design — different tools)
- Response payload size (full state snapshots are a known trade-off, mitigated by delta events)
- Missing authentication on endpoints (Phase 1 is local-only, single-user)
- Missing pagination on session/message lists (not needed at current scale)
