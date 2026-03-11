# Feature Brief: Gemini CLI Integration

This brief tracks Gemini as a first-class TermAl agent.

Reference: [`agent-integration-comparison.md`](./agent-integration-comparison.md)

## Problem

Only Claude and Codex are wired through session creation, runtime spawning,
message dispatch, and frontend rendering. Gemini CLI is a widely-used open-source
agent with strong tool support that is missing entirely from TermAl.

## Integration options

Gemini CLI offers **two** integration paths. The recommended path depends on how
mature ACP support is at the time of implementation.

### Option A — `stream-json` headless mode (simpler, unidirectional)

Spawn the CLI in non-interactive mode and parse its JSONL stdout. This is the
simplest path and works today.

```bash
gemini -p "<prompt>" --output-format stream-json \
  --approval-mode yolo \
  --model auto \
  --sandbox
```

**Pros:** Simple to implement, stable API, no experimental flags.
**Cons:** Unidirectional (stdout only), no mid-turn approval, no session resume
within a single process, must re-spawn per turn.

### Option B — `--experimental-acp` mode (richer, bidirectional)

> Source: https://geminicli.com/docs/cli/cli-reference/

Run Gemini as a full ACP (Agent Client Protocol) server over stdio. This gives
the same JSON-RPC 2.0 bidirectional protocol that Cursor uses, making it
structurally identical to the Cursor adapter.

```bash
gemini --experimental-acp
```

**Pros:** Bidirectional JSON-RPC, permission handling, session management,
same protocol as Cursor (shared adapter logic possible).
**Cons:** Experimental flag — may change. Requires more upfront protocol work.

### Recommendation

Start with **Option A** (`stream-json`) for an initial working integration, then
migrate to **Option B** (`--experimental-acp`) once that flag stabilizes or if
bidirectional approval is needed. The ACP path could potentially share adapter
code with the Cursor integration.

---

## Option A — stream-json protocol

### Transport

| Property | Value |
|----------|-------|
| Transport | Spawned child process per turn |
| Output | stdout, newline-delimited JSON (JSONL) |
| Input | Single prompt via `-p` flag (no stdin interaction) |
| Direction | Unidirectional (server → client only) |

### Launch command

```bash
gemini -p "<prompt>" \
  --output-format stream-json \
  --approval-mode <yolo|auto_edit|default> \
  --model <auto|pro|flash|flash-lite> \
  -r <session-index-or-"latest">     # optional: resume session
```

### CLI flags reference

| Flag | Short | Description |
|------|-------|-------------|
| `--prompt` | `-p` | Prompt text; forces non-interactive mode |
| `--output-format` | `-o` | `text`, `json`, or `stream-json` |
| `--model` | `-m` | `auto`, `pro`, `flash`, `flash-lite` |
| `--approval-mode` | | `default`, `auto_edit`, `yolo` |
| `--sandbox` | `-s` | Run in sandboxed environment |
| `--resume` | `-r` | Resume session by index or `"latest"` |
| `--extensions` | `-e` | Comma-separated list of enabled extensions |
| `--allowed-mcp-server-names` | | Filter allowed MCP servers |
| `--include-directories` | | Extra workspace directories |
| `--debug` | `-d` | Verbose logging |

### Event types

Each line of stdout is a JSON object with a `type` field:

#### `init`

Emitted once at the start.

```json
{
  "type": "init",
  "timestamp": "2025-10-10T12:00:00.000Z",
  "session_id": "abc123",
  "model": "gemini-2.0-flash-exp"
}
```

#### `message`

User echo and assistant response chunks.

```json
{
  "type": "message",
  "role": "assistant",
  "content": "I'll investigate the auth middleware...",
  "delta": true,
  "timestamp": "2025-10-10T12:00:04.000Z"
}
```

When `delta` is `true`, the content is an incremental chunk (map to
`TextDeltaEvent`). When `false` or absent, it is a complete message.

#### `tool_use`

Tool invocation request.

```json
{
  "type": "tool_use",
  "tool_name": "Bash",
  "tool_id": "bash-123",
  "parameters": { "command": "npm test" },
  "timestamp": "2025-10-10T12:00:02.000Z"
}
```

#### `tool_result`

Output from an executed tool.

```json
{
  "type": "tool_result",
  "tool_id": "bash-123",
  "status": "success",
  "output": "Tests passing: 12/12\n",
  "timestamp": "2025-10-10T12:00:03.000Z"
}
```

#### `error`

Non-fatal warnings and system errors.

```json
{
  "type": "error",
  "message": "Rate limit exceeded, retrying...",
  "timestamp": "2025-10-10T12:00:06.000Z"
}
```

#### `result`

Final event — marks end of the turn.

```json
{
  "type": "result",
  "status": "success",
  "stats": {
    "total_tokens": 250,
    "input_tokens": 50,
    "output_tokens": 200,
    "duration_ms": 3000,
    "tool_calls": 1
  },
  "timestamp": "2025-10-10T12:00:05.000Z"
}
```

### Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success |
| `1` | General error or API failure |
| `42` | Input error (invalid prompt or arguments) |
| `53` | Turn limit exceeded |

### Session persistence

| Property | Value |
|----------|-------|
| Storage format | Single `.json` per session |
| Location | `~/.gemini/tmp/<project>/chats/session-{ts}-{id}.json` |
| Resume mechanism | `--resume <index>` or `--resume latest` |
| Session discovery | Scan `chats/` directory |
| Cross-session memory | `GEMINI.md` project instructions and `~/.gemini/memory.md` |
| Ignore file | `.geminiignore` |

---

## Option B — ACP mode (experimental)

### Transport

| Property | Value |
|----------|-------|
| Transport | `stdio` (stdin/stdout) |
| Envelope | JSON-RPC 2.0 |
| Framing | Newline-delimited JSON |
| Direction | Bidirectional |

### Launch command

```bash
gemini --experimental-acp
```

### Protocol

Gemini's ACP mode implements the same Agent Client Protocol used by Cursor CLI.
The core methods are identical:

- `initialize` — handshake with protocol version and client capabilities
- `session/new` — create a new session
- `session/load` — resume an existing session
- `session/prompt` — send a user prompt
- `session/cancel` — cancel the current turn
- `session/update` — streaming notifications (server → client)
- `session/request_permission` — tool approval requests (server → client)

This means a **shared ACP adapter** could serve both Cursor and Gemini, with
agent-specific differences isolated to:
- Executable resolution (`cursor` vs `gemini`)
- Launch arguments (`agent acp` vs `--experimental-acp`)
- Authentication flow
- Extended notification methods (Cursor has `cursor/*`, Gemini may differ)

### Authentication

```bash
# OAuth (interactive browser flow)
gemini --experimental-acp   # prompts for login on first use

# API key
GEMINI_API_KEY=... gemini --experimental-acp

# Vertex AI
# Configured via gcloud credentials
```

---

## Mapping to TermAl concepts

### Option A mapping (stream-json)

| Gemini event | TermAl equivalent | Notes |
|--------------|-------------------|-------|
| `init` | Session metadata | Extract `session_id`, `model` |
| `message` (delta) | `TextDeltaEvent` | Streaming text into chat bubble |
| `message` (complete) | `TextMessage` | Full assistant response |
| `tool_use` | `CommandMessage` (status: running) | Show tool invocation |
| `tool_result` | `CommandMessage` (status: success/error) | Show tool output |
| `error` | Error card or inline warning | Surface in chat |
| `result` | Turn completion | End turn, update session status |

### Option B mapping (ACP)

Same as the Cursor ACP mapping — see
[`cursor-cli-integration.md`](./cursor-cli-integration.md#mapping-to-termal-concepts).

## Approval model

### Stream-json mode

Gemini's non-interactive mode has a **known limitation**: it cannot prompt for
tool approval mid-turn. The workarounds are:

| `--approval-mode` | Behavior |
|-------------------|----------|
| `yolo` | Auto-approve everything (no approval cards in TermAl) |
| `auto_edit` | Auto-approve file edits, prompt for dangerous commands |
| `default` | Prompt for approval — **hangs in non-interactive mode** |

For stream-json integration, TermAl should default to `auto_edit` or `yolo` and
surface the choice in session settings. True interactive approval requires
ACP mode (Option B).

### ACP mode

Full bidirectional approval via `session/request_permission`, identical to the
Cursor flow. TermAl can intercept and surface approval cards.

## Backend tasks

### Enum extensions

Add a `Gemini` variant to each of these enums in `src/main.rs`:

- `Agent` — `Gemini` (with `name()` → `"Gemini"`, `avatar()` → `"GM"`)
- `SessionRuntime` — `Gemini(GeminiRuntimeHandle)`
- `RuntimeToken` — `Gemini(String)`
- `KillableRuntime` — `Gemini(GeminiRuntimeHandle)`
- `TurnDispatch` — `PersistentGemini { command, sender, session_id }`

### New types

- `GeminiRuntimeHandle` — `runtime_id`, `input_tx`, `process`
- `GeminiRuntimeCommand` — `Prompt(GeminiPromptCommand)` | (ACP: `ApprovalResponse(…)`)
- `GeminiPromptCommand` — `prompt`, `approval_mode`, `model`, `session_index`
- `GeminiApprovalMode` — `Default` | `AutoEdit` | `Yolo`

### New functions

- `resolve_gemini_executable()` — `find_command_on_path("gemini")`
- `spawn_gemini_runtime()` — spawn CLI with appropriate flags; for stream-json
  mode, spawn per-turn; for ACP mode, spawn once as a long-lived process
- `handle_gemini_stream_event()` — parse JSONL events (`init`, `message`,
  `tool_use`, `tool_result`, `error`, `result`), map to TermAl message types
- (ACP mode) `handle_gemini_acp_message()` — reuse or share with Cursor ACP
  adapter

### Existing function changes

- `deliver_turn_dispatch()` — add `PersistentGemini` arm
- `dispatch_turn()` / `start_turn_on_record()` — handle `Agent::Gemini`
- `update_session_settings()` — handle Gemini approval mode and model selection
- Agent CLI arg parsing — accept `"gemini"` in `Agent::from_str`

### Process model considerations

Unlike Claude and Codex which are long-lived processes, the **stream-json** path
spawns a new process per turn. This means:

- No `SessionRuntime` handle between turns (or use a lightweight "idle" state)
- Session resume via `--resume latest` on each new turn
- Consider caching the `session_id` from the `init` event for resume tracking

The **ACP** path avoids this — a single long-lived process like Claude/Codex.

## Frontend tasks

- Update `AgentType` in `types.ts`: `"Claude" | "Codex" | "Gemini"`
- Add Gemini to the agent selector dropdown in session creation
- Add settings for Gemini sessions:
  - Model selector (`auto`, `pro`, `flash`, `flash-lite`)
  - Approval mode selector (`default`, `auto_edit`, `yolo`)
- Render Gemini sessions with agent-colored badge and avatar
- Map `tool_use` / `tool_result` events to command cards with streaming output
- (ACP mode) Reuse the approval card UI with the same flow as Cursor

## Testing

- Add unit tests for JSONL event parsing (`init`, `message`, `tool_use`,
  `tool_result`, `error`, `result`)
- Test turn lifecycle: spawn → parse init → stream messages → tool calls →
  result → turn complete
- Test session resume via `--resume`
- Test approval mode settings (verify correct CLI flags are passed)
- Test error handling: non-zero exit codes, malformed JSON lines, stderr output
- (ACP mode) Reuse ACP adapter tests with Gemini-specific setup

## Open questions

1. **ACP stability** — `--experimental-acp` may change or break across Gemini CLI
   releases. Should we gate ACP mode behind a TermAl feature flag?
2. **Per-turn vs long-lived** — For stream-json mode, is spawning per turn
   acceptable for UX, or is the startup latency too high?
3. **Shared ACP adapter** — Can we build a single generic ACP adapter that both
   Cursor and Gemini use, parameterized by executable + launch args + auth flow?
4. **File diff rendering** — Does Gemini's `tool_use`/`tool_result` provide enough
   structure to render diff cards, or do we need to parse patch content from
   tool output?
5. **Sandbox mode** — Gemini's `--sandbox` flag enables a container sandbox. Should
   TermAl surface this as a session setting alongside approval mode?
6. **Authentication flow** — Gemini supports OAuth, API key, and Vertex AI auth.
   How should TermAl handle the initial auth setup? Should it detect missing
   credentials and prompt?
7. **Extension system** — Gemini has a plugin/extension model. Should TermAl allow
   configuring which extensions are enabled per session?
