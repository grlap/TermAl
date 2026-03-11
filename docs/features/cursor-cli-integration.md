# Feature Brief: Cursor CLI Integration

This brief tracks Cursor as a first-class TermAl agent via its ACP (Agent Client
Protocol) mode.

Reference: [`agent-integration-comparison.md`](./agent-integration-comparison.md)

## Problem

Only Claude and Codex are wired through session creation, runtime spawning,
message dispatch, and frontend rendering. Cursor CLI offers a rich terminal agent
with its own protocol that many developers already use.

## Why Cursor fits well

Cursor CLI's `agent acp` subcommand exposes a **JSON-RPC 2.0 over stdio** server
that is structurally very close to the Codex `app-server` adapter TermAl already
implements. This means the existing Codex adapter can serve as a near-direct
template for the Cursor adapter.

## Protocol overview — ACP

> Source: https://cursor.com/docs/cli/acp

### Transport

| Property | Value |
|----------|-------|
| Transport | `stdio` (stdin/stdout) |
| Envelope | JSON-RPC 2.0 |
| Framing | Newline-delimited JSON (one message per line) |
| Logs | stderr (ignored by protocol) |

### Launch command

```bash
cursor agent acp
```

### Authentication

Authenticate before first use with one of:

```bash
cursor agent login                    # interactive browser login
cursor agent acp --api-key <key>      # API key
cursor agent acp --auth-token <token> # auth token
# or environment variables:
CURSOR_API_KEY=...
CURSOR_AUTH_TOKEN=...
```

### Lifecycle

```
Client                                  cursor agent acp
  │                                           │
  │──── initialize ──────────────────────────>│
  │<─── initialize result ───────────────────│
  │                                           │
  │──── authenticate ────────────────────────>│
  │<─── authenticate result ─────────────────│
  │                                           │
  │──── session/new ─────────────────────────>│
  │<─── { sessionId } ──────────────────────│
  │                                           │
  │──── session/prompt ──────────────────────>│
  │<─── session/update (notification) ───────│  (streaming, repeats)
  │<─── session/request_permission ──────────│  (if tool needs approval)
  │──── permission response ─────────────────>│
  │<─── session/update (notification) ───────│  (streaming continues)
  │<─── session/prompt result ───────────────│  { stopReason }
  │                                           │
  │──── session/prompt (next turn) ──────────>│
  │     ...                                   │
  │                                           │
  │──── session/cancel (optional) ───────────>│
```

### Core methods

#### `initialize`

Establishes protocol version and capabilities.

```jsonc
// Client → Server
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "initialize",
  "params": {
    "protocolVersion": "2025-01-01",
    "clientInfo": { "name": "termal", "version": "0.1.0" },
    "clientCapabilities": {
      "fs": { "readTextFile": true, "writeTextFile": true },
      "terminal": true
    }
  }
}
```

#### `authenticate`

```jsonc
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "authenticate",
  "params": { "methodId": "cursor_login" }
}
```

#### `session/new`

Creates a new conversation session.

```jsonc
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "session/new",
  "params": {
    "cwd": "/projects/my-app",
    "mcpServers": []
  }
}
// Response: { "sessionId": "..." }
```

#### `session/load`

Resumes an existing session by ID.

```jsonc
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "session/load",
  "params": { "sessionId": "<previous-session-id>" }
}
```

#### `session/prompt`

Sends a user message. Returns when the turn completes.

```jsonc
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "session/prompt",
  "params": {
    "sessionId": "<session-id>",
    "prompt": [{ "type": "text", "text": "Fix the auth middleware" }]
  }
}
// Response: { "stopReason": "end_turn" }
```

#### `session/cancel`

Interrupts the current turn.

```jsonc
{
  "jsonrpc": "2.0",
  "id": 5,
  "method": "session/cancel",
  "params": { "sessionId": "<session-id>" }
}
```

### Streaming — `session/update` notifications

While a prompt is being processed, the server emits JSON-RPC notifications
(no `id` field):

```jsonc
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionUpdate": "agent_message_chunk",
    "content": { "text": "I'll investigate the auth..." }
  }
}
```

### Permission requests — `session/request_permission`

When a tool requires approval, the server sends a JSON-RPC request:

```jsonc
// Server → Client
{
  "jsonrpc": "2.0",
  "id": 100,
  "method": "session/request_permission",
  "params": {
    "toolName": "edit_file",
    "description": "Edit src/auth.ts",
    "options": ["allow-once", "allow-always", "reject-once"]
  }
}

// Client → Server
{
  "jsonrpc": "2.0",
  "id": 100,
  "result": {
    "outcome": { "outcome": "selected", "optionId": "allow-once" }
  }
}
```

### Extended notification methods

Cursor advertises additional notification methods for richer UX:

| Method | Purpose |
|--------|---------|
| `cursor/ask_question` | Multiple-choice prompts to the user |
| `cursor/create_plan` | Plan approval flow |
| `cursor/update_todos` | Progress/task notifications |
| `cursor/task` | Sub-agent completion events |
| `cursor/generate_image` | Image output notifications |

### Agent modes

Cursor supports three modes that can be selected at session creation:

| Mode | Description |
|------|-------------|
| `agent` | Full tool access — reads, writes, commands |
| `plan` | Read-only planning — proposes changes without executing |
| `ask` | Q&A only — explores code, answers questions |

## Mapping to TermAl concepts

| Cursor concept | TermAl equivalent | Notes |
|----------------|-------------------|-------|
| `session/update` `agent_message_chunk` | `TextDeltaEvent` | Streaming text into chat bubble |
| `session/request_permission` | `ApprovalMessage` | Maps to approve/reject/approve-for-session |
| `session/cancel` | Turn interrupt | Same as Claude's `control_request` interrupt |
| `session/load` | Session resume | Like `--resume` for Claude, `thread/resume` for Codex |
| `cursor/update_todos` | Could map to a new message type | Optional enhancement |
| Modes (agent/plan/ask) | New Cursor-specific session setting | Surface in session creation UI |

## Backend tasks

### Enum extensions

Add a `Cursor` variant to each of these enums in `src/main.rs`:

- `Agent` — `Cursor` (with `name()` → `"Cursor"`, `avatar()` → `"CR"`)
- `SessionRuntime` — `Cursor(CursorRuntimeHandle)`
- `RuntimeToken` — `Cursor(String)`
- `KillableRuntime` — `Cursor(CursorRuntimeHandle)`
- `TurnDispatch` — `PersistentCursor { command, sender, session_id }`

### New types

- `CursorRuntimeHandle` — `runtime_id`, `input_tx: Sender<CursorRuntimeCommand>`, `process`
- `CursorRuntimeCommand` — `Prompt(CursorPromptCommand)` | `ApprovalResponse(…)` | `Cancel`
- `CursorPromptCommand` — `prompt: String`, `mode: CursorMode`, `session_id: String`, `attachments`
- `CursorPendingApproval` — `request_id`, `tool_name`, `description`, `options`
- `CursorMode` — `Agent` | `Plan` | `Ask`

### New functions

- `resolve_cursor_executable()` — `find_command_on_path("cursor")`
- `spawn_cursor_runtime()` — spawn `cursor agent acp`, set up stdin/stdout/stderr
  pipes, run writer/reader/stderr/wait threads (follow `spawn_codex_runtime` pattern)
- `cursor_initialize_handshake()` — send `initialize` + `authenticate`, await responses
- `cursor_create_session()` — send `session/new` or `session/load`
- `handle_cursor_acp_message()` — parse incoming JSON-RPC messages, dispatch:
  - `session/update` → map to `TextDeltaEvent` / message cards
  - `session/request_permission` → map to `ApprovalMessage`
  - `cursor/*` notifications → map to appropriate message types
- `send_cursor_json_rpc()` — write JSON-RPC message to stdin, manage pending request map

### Existing function changes

- `deliver_turn_dispatch()` — add `PersistentCursor` arm
- `dispatch_turn()` / `start_turn_on_record()` — handle `Agent::Cursor`
- `update_session_settings()` — handle Cursor mode selection
- Agent CLI arg parsing — accept `"cursor"` in `Agent::from_str`

## Frontend tasks

- Update `AgentType` in `types.ts`: `"Claude" | "Codex" | "Cursor"`
- Add Cursor to the agent selector dropdown in session creation
- Add a mode selector (agent / plan / ask) shown when Cursor is selected
- Render Cursor sessions with agent-colored badge and avatar
- Map Cursor approval responses to the existing approval card UI
  (`allow-once` → Approve, `allow-always` → Approve for session,
  `reject-once` → Reject)

## Testing

- Add unit tests for ACP JSON-RPC message parsing (init, session/update,
  session/request_permission, extended cursor/* notifications)
- Add turn lifecycle tests: spawn → init → session/new → prompt → stream →
  permission → approve → stream → result → next prompt
- Test session resume via `session/load`
- Test cancel via `session/cancel`
- Test mode switching (agent/plan/ask)
- Mock Cursor ACP server for integration tests

## Open questions

1. **Exact `session/update` payload shapes** — need to test against a live
   `cursor agent acp` process to catalogue all `sessionUpdate` variants beyond
   `agent_message_chunk` (file diffs, command executions, thinking, etc.).
2. **Image attachment support** — does `session/prompt` accept image content blocks?
3. **MCP server passthrough** — should TermAl forward its own MCP config to Cursor
   via the `mcpServers` param, or let Cursor use its own `.cursor/mcp.json`?
4. **Cloud handoff** — Cursor supports pushing a session to the cloud via `&` prefix.
   Should TermAl surface this capability?
5. **Subscription model** — Cursor uses its own subscription (not a raw API key).
   How does this affect multi-user or team scenarios?
