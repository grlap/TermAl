# TermAl — Historical Product Specification

> Historical design draft from the original desktop-first/Tauri planning phase.

> Current implementation note: TermAl now ships as an axum backend plus a React frontend, with
> Claude, Codex, Gemini, and Cursor integrations. For the current architecture and active feature
> briefs, see `docs/architecture.md`, `docs/bugs.md`, and `docs/features/`.

**TermAl** — Terminal meets AI. The capital A makes the double meaning explicit.
Domain: `termal.app`

---

## Vision

This document captures the original product direction: a fast interface for running and interacting
with multiple AI coding agent sessions side by side, with structured rendering for diffs, markdown,
tool calls, and approvals. The core UX intent still applies, but the delivery architecture has
changed substantially since this draft was written.

---

# Original Phase 1 — Desktop App (Local)

## Goal

Build and polish the core experience on desktop first. Everything runs on the same machine. No relay, no remote access, no mobile. Just a fast, focused app for controlling multiple agent sessions from a single interface.

---

## What It Was

At the time of this draft, the planned implementation was a Tauri desktop app that:

- Manages multiple named agent sessions (contacts)
- Spawns Claude Code and Codex as long-lived child processes with bidirectional JSON protocols
- Renders responses as chat messages — not raw terminal output
- Shows diffs, markdown, tool calls, and command executions as first-class UI elements
- Lets you approve or reject agent actions (file writes, shell commands)

---

## Core Concepts

### Sessions (Contacts)

Each session is a named agent process scoped to a working directory.

```
Sessions list
├── 🔧 Backend        Claude Code  /projects/api       ● active
├── 🎨 Frontend       Claude Code  /projects/web       ● idle
├── 🧪 Test Runner    Codex        /projects/api       ● idle
└── 🗄️ DB Migrations  Codex        /projects/infra     ● idle
```

- Each session has: name, avatar/emoji, agent type, working directory, model
- Sessions persist across app restarts (process is re-attached or restarted)
- Multiple sessions can run simultaneously
- Sessions are shown in a sidebar like a chat contact list

### Message Types

Responses are not raw text. Each event from the agent becomes a typed message card in the chat thread.

| Type | Rendered as |
|---|---|
| Text response | Chat bubble |
| File edit | Diff card (before/after, file path) |
| File create | Diff card (new file) |
| Shell command | Command card (command + output, expandable) |
| Markdown output | Rendered markdown block |
| Todo list | Task list card |
| Approval request | Interactive card — Approve / Reject |
| Error | Error card |
| Thinking | Collapsible reasoning block |

---

## Agent Integration

### Claude Code

> Based on reverse-engineering the VS Code extension (anthropic.claude-code). This is the same protocol the official IDE integration uses.

**Process model:** Long-lived child process with bidirectional NDJSON over stdin/stdout. One process per session — no respawning per message.

**Invocation:**
```bash
claude --output-format stream-json --verbose --input-format stream-json \
  --permission-prompt-tool stdio \
  --resume <session-id>          # optional: resume existing session
```

Key optional flags:
```
--model <model>                  # model selection
--max-turns <N>                  # limit agent turns
--allowedTools <csv>             # restrict available tools
--disallowedTools <csv>          # block specific tools
--thinking adaptive|disabled     # thinking mode
--max-thinking-tokens <N>        # thinking budget
--permission-mode <mode>         # permission policy
```

Environment variables:
```
CLAUDE_CODE_ENTRYPOINT=termal    # identify our app
```

**Protocol: Newline-Delimited JSON (NDJSON)**

All communication is `JSON.stringify(msg) + "\n"` in both directions.

**Step 1 — Initialize (app → claude, on spawn):**
```json
{
  "request_id": "<uuid>",
  "type": "control_request",
  "request": {
    "subtype": "initialize",
    "hooks": {},
    "systemPrompt": "",
    "appendSystemPrompt": ""
  }
}
```

Claude responds with `control_response` containing `pid`, available `commands`, `models`, etc.

**Step 2 — Send user message (app → claude):**
```json
{
  "type": "user",
  "session_id": "",
  "message": {
    "role": "user",
    "content": [{"type": "text", "text": "Fix the auth middleware"}]
  },
  "parent_tool_use_id": null
}
```

**Step 3 — Receive events (claude → app, streamed):**

| Message type | Description |
|---|---|
| `assistant` | Assistant message with content blocks (text, tool_use) |
| `result` | Final result — marks end of a turn |
| `control_request` | Claude asking for permission (tool approval) |
| `control_cancel_request` | Claude canceling a previous permission request |
| `keep_alive` | Heartbeat, ignore |

Assistant messages contain content blocks:
```json
{
  "type": "assistant",
  "message": {
    "role": "assistant",
    "content": [
      {"type": "text", "text": "I'll fix the auth middleware..."},
      {"type": "tool_use", "id": "toolu_123", "name": "Edit",
       "input": {"file_path": "src/auth.ts", "old_string": "...", "new_string": "..."}},
      {"type": "tool_result", "tool_use_id": "toolu_123", "content": "File edited"}
    ]
  }
}
```

**Step 4 — Permission flow (bidirectional):**

When Claude wants to use a tool that requires approval:
```json
// claude → app
{
  "type": "control_request",
  "request_id": "<id>",
  "request": {
    "subtype": "can_use_tool",
    "tool_name": "Bash",
    "input": {"command": "npm test"}
  }
}

// app → claude
{
  "type": "control_response",
  "response": {
    "subtype": "success",
    "request_id": "<matching-id>",
    "response": {"allowed": true}
  }
}
```

**Step 5 — Control commands (app → claude, anytime):**

```json
// Interrupt current generation
{"request_id": "<id>", "type": "control_request",
 "request": {"subtype": "interrupt"}}

// Change model mid-session
{"request_id": "<id>", "type": "control_request",
 "request": {"subtype": "set_model", "model": "claude-sonnet-4-6"}}

// Change permission mode
{"request_id": "<id>", "type": "control_request",
 "request": {"subtype": "set_permission_mode", "mode": "..."}}
```

**Multi-turn:** After receiving a `result` message, send another `user` message on the same stdin. The process stays alive and maintains full context.

**Session management:**
- New session: no session flags
- Resume: `--resume <session-id>`
- Continue last: `--continue`
- Session history stored in `<project>/.claude/sessions/<session-id>.jsonl`

**Pricing:** Uses Claude subscription (Max plan) — not per-token API billing.

### Codex

**Two invocation modes:**

**Simple (exec --json):**
```bash
codex exec --json "<message>" --cd /projects/api
```

**Rich (app-server) — preferred:**
```bash
codex app-server   # stdio JSON-RPC, bidirectional
```

**Key events from app-server:**
```json
// Thread lifecycle
{ "method": "thread/started", "params": { "threadId": "..." }}

// File change
{ "method": "item/fileChange/outputDelta",
  "params": { "patch": "...", "filePath": "src/auth.ts" }}

// Command execution (streaming stdout)
{ "method": "item/commandExecution/outputDelta",
  "params": { "delta": "Tests passing: 12/12\n" }}

// Approval request (bidirectional — app must respond)
{ "method": "serverRequest/approval",
  "params": { "type": "fileChange", "filePath": "src/auth.ts" }}
```

**Approval response (app → Codex):**
```json
{ "id": "...", "result": { "decision": "accept" }}
// or: "decline", "acceptForSession"
```

**Pricing:** Uses OpenAI API key — per-token billing. User provides their own key.

---

## Application Architecture

```
Electron / Tauri App
├── Main process (Node.js / Rust)
│   ├── Session Manager
│   │   ├── Spawns agent processes (child_process / Command)
│   │   ├── Parses JSON event streams per session
│   │   ├── Maintains session state (idle / thinking / streaming / awaiting approval)
│   │   └── Persists session config to disk
│   ├── Claude Code Adapter
│   │   └── Bidirectional NDJSON over stdin/stdout
│   ├── Codex Adapter
│   │   └── Speaks JSON-RPC with app-server
│   └── IPC bridge to renderer
└── Renderer (React)
    ├── Sidebar — session list
    ├── Chat view — message thread per session
    ├── Message renderer — typed cards
    ├── Diff viewer
    ├── Markdown renderer
    └── Approval UI
```

**Tech stack: Tauri + React + TypeScript**

Tauri is chosen over Electron for TermAl specifically because:
- The Rust backend maps directly to the core workload: spawning processes, parsing JSON streams, managing concurrent sessions, and Phase 2 networking
- Rust's async concurrency model (`tokio`) is ideal for handling multiple simultaneous agent streams without interference
- Significantly lighter binary than Electron (no bundled Chromium)
- Phase 2 relay server is naturally the same Rust codebase — no context switch

| Layer | Choice | Reason |
|---|---|---|
| Desktop shell | Tauri 2 | Rust backend, lightweight, no bundled browser |
| Backend language | Rust + Tokio | Async process management, JSON stream parsing |
| Frontend language | TypeScript + React | Standard, fast iteration |
| Styling | Tailwind CSS | Utility-first, no design system overhead |
| Diff rendering | `diff2html` | Battle-tested unified diff rendering |
| Markdown | `react-markdown` + `remark-gfm` | Full GFM support |
| State | Zustand | Simple, minimal boilerplate |
| IPC | Tauri commands + events | Typed Rust ↔ TypeScript bridge |
| Persistence | JSON files via `serde_json` | Session config, history |
| Build | Vite | Fast HMR for frontend |
| Package manager | Bun | Fast installs |

**Project structure:**

```
termal/
├── src-tauri/                  # Rust backend
│   ├── src/
│   │   ├── main.rs
│   │   ├── session_manager.rs  # Spawn, track, kill agent processes
│   │   ├── claude_adapter.rs   # Parse --output-format stream-json
│   │   ├── codex_adapter.rs    # JSON-RPC with codex app-server
│   │   ├── event_parser.rs     # Typed event structs (diff, command, text...)
│   │   ├── persistence.rs      # Session config + history to disk
│   │   └── ipc.rs              # Tauri command handlers
│   └── Cargo.toml
├── src/                        # React frontend
│   ├── App.tsx
│   ├── components/
│   │   ├── Sidebar.tsx         # Session list
│   │   ├── ChatView.tsx        # Message thread
│   │   ├── MessageCard.tsx     # Dispatcher for message types
│   │   ├── DiffCard.tsx        # File diff rendering
│   │   ├── CommandCard.tsx     # Shell command + output
│   │   ├── MarkdownCard.tsx    # Rendered markdown
│   │   └── ApprovalCard.tsx    # Approve/reject for Codex
│   ├── store/
│   │   └── sessions.ts         # Zustand session state
│   └── types/
│       └── events.ts           # TypeScript types matching Rust structs
├── package.json
└── vite.config.ts
```

---

## UI Screens

### 1. Session Sidebar

```
┌─────────────────────┐
│  + New Session      │
├─────────────────────┤
│ 🔧 Backend          │
│   Claude Code       │
│   ● active          │
├─────────────────────┤
│ 🎨 Frontend         │
│   Claude Code       │
│   ○ idle            │
├─────────────────────┤
│ 🧪 Tests            │
│   Codex             │
│   ○ idle            │
└─────────────────────┘
```

- Click session → opens chat view
- Color/pulse indicator: active (streaming), idle, awaiting approval, error
- Last message preview under session name

### 2. Chat View

```
┌──────────────────────────────────────────────────────┐
│ 🔧 Backend Claude                          ● active  │
├──────────────────────────────────────────────────────┤
│                                                      │
│  You                                      10:42      │
│  Fix the auth middleware, it's throwing              │
│  401s on refresh tokens                              │
│                                                      │
│  Claude Code                              10:42      │
│  I'll investigate the auth middleware...             │
│  ┌─────────────────────────────────────┐            │
│  │ 📖 Reading src/middleware/auth.ts   │            │
│  └─────────────────────────────────────┘            │
│  ┌─────────────────────────────────────┐            │
│  │ 📝 Edited src/middleware/auth.ts    │            │
│  │ +12 -4 lines              [view ▼]  │            │
│  └─────────────────────────────────────┘            │
│  ┌─────────────────────────────────────┐            │
│  │ $ npm test                          │            │
│  │ ✅ 24 tests passing        [view ▼] │            │
│  └─────────────────────────────────────┘            │
│  The issue was the token expiry check was            │
│  comparing unix timestamps in different              │
│  timezones. Fixed and tests pass.                    │
│                                                      │
├──────────────────────────────────────────────────────┤
│  Message Backend Claude...              [Send ↵]     │
└──────────────────────────────────────────────────────┘
```

### 3. Diff Card (expanded)

```
┌─────────────────────────────────────────────────────┐
│ 📝 src/middleware/auth.ts                [collapse]  │
├─────────────────────────────────────────────────────┤
│  @@ -42,8 +42,12 @@                                  │
│  - const isExpired = token.exp < Date.now()          │
│  + const isExpired = token.exp < Date.now() / 1000   │
│  + // Token exp is in seconds, Date.now() in ms      │
└─────────────────────────────────────────────────────┘
```

### 4. Approval Card (Codex)

```
┌─────────────────────────────────────────────────────┐
│ ⚠️  Codex wants to execute a command                 │
│                                                      │
│  $ npm install express-rate-limit                    │
│                                                      │
│  [Approve]  [Approve for session]  [Reject]         │
└─────────────────────────────────────────────────────┘
```

### 5. New Session Dialog

Fields:
- Name (free text)
- Emoji/avatar picker
- Agent type: Claude Code / Codex
- Working directory (folder picker)
- Model (dropdown, populated per agent)
- Allowed tools (checkboxes)

---

## Session Lifecycle

```
Create session
    → Config saved to disk
    → Process spawned on first message (lazy)

Send message
    → Agent process spawned (if not running)
    → Initialize handshake (control_request → control_response)
    → User message written to stdin as NDJSON
    → Events streamed back on stdout → parsed → rendered in chat

Multi-turn
    → Process stays alive between messages
    → New user messages written to same stdin
    → Full context maintained in-process

App restart
    → Claude Code: respawn with --resume <session-id>
    → Codex: new app-server + thread/resume
    → UI state reconstructed from session history files

Session closed
    → Process killed (SIGTERM)
    → History retained on disk
```

---

## Data Persistence

Stored locally in app data directory:

```
~/.termal/
├── sessions.json          # Session configs (name, agent, workdir, model)
├── history/
│   ├── <session-id>.jsonl # Message history per session
└── settings.json          # App preferences
```

---

## Phase 1 MVP Milestones

### Milestone 1 — Shell & Sessions
- [ ] Tauri app with sidebar + chat layout
- [ ] New session dialog
- [ ] Session config persisted to disk
- [ ] Claude Code process spawned on message send
- [ ] Raw text streaming into chat bubble

### Milestone 2 — Structured Rendering
- [ ] Parse stream-json events from Claude Code
- [ ] File edit → diff card
- [ ] Shell command → command card with output
- [ ] Markdown rendering
- [ ] Tool call indicators ("Reading file...", "Running tests...")

### Milestone 3 — Codex Integration
- [ ] Codex app-server adapter
- [ ] Approval card with accept/reject
- [ ] Codex file change diff rendering
- [ ] Command execution streaming output

### Milestone 4 — Polish
- [ ] Session resume on app restart
- [ ] Multi-session parallel view (split pane optional)
- [ ] Session status indicators
- [ ] Message history scrollback
- [ ] Settings screen (API keys, defaults)
- [ ] Error handling and process recovery

---

## Phase 1 Success Criteria

- [ ] Can run 3+ parallel sessions without interference
- [ ] Claude Code diffs render correctly for edits and new files
- [ ] Codex approval flow works end to end
- [ ] Session survives app restart with context intact
- [ ] Feels faster and cleaner than looking at a raw terminal

---

# Phase 2 — Remote Architecture (Draft)

> Short general overview. Design in detail after Phase 1 is stable.

## Goal

Access all sessions from any device — phone, tablet, another PC — with the same chat UI experience as Phase 1.

## Core Addition: Relay Server

```
Phone / Tablet
    ↕ WSS
Relay Server (small VPS — $5–6/month Hetzner)
    ↕ WSS (outbound from dev machines)
Dev Machine A  ←→  Agent processes
Dev Machine B  ←→  Agent processes
```

Dev machines connect **outbound** to the relay. No inbound ports, no firewall rules, works behind home NAT.

## Components to Add

**Relay server** (new)
- WebSocket hub — routes messages between machines and clients
- Session registry — unified list of all sessions across all machines
- Auth — token per machine agent, token per client device
- Thin: no business logic, just routing

**Agent daemon** (replaces direct process spawn)
- Runs as a background service on each dev machine
- Manages local PTY/child processes
- Connects outbound to relay on startup
- Registers its sessions with the relay

**Mobile client** (new)
- React Native or PWA
- Same chat UI as desktop
- Touch-optimized: larger tap targets, bottom input bar
- Notifications: "Claude Code finished your task"

**Desktop app changes**
- Minimal: add a "connect to relay" setting
- All session management stays identical

## What the Phone Experience Looks Like

```
📱 Sessions
─────────────────────────────
🖥️ Workstation
  🔧 Backend Claude     ● active
  🎨 Frontend Claude    ○ idle

🖥️ Home Lab
  🗄️ DB Migrations      ● running
─────────────────────────────
```

Tap any session → full chat view → same diff cards, approval flow, markdown — just on a 6" screen.

## Security Model

- Relay never stores message content — pure routing
- All traffic WSS (TLS)
- Machine agents authenticate with a per-machine secret
- Client devices authenticate with a user token
- Self-hostable relay (your own VPS) — no third-party cloud

## Open Questions for Phase 2 Design

- Push notifications: how to deliver "turn complete" to phone when app is backgrounded?
- Offline: what happens when phone loses connection mid-session?
- Multi-user: could a second engineer connect and observe a session?
- Relay hosting: self-hosted only, or offer a hosted relay for convenience?
- Mobile approval UX: biometric confirmation before approving destructive commands?

---

## Overall Roadmap

```
Phase 1
  Desktop app, local only
  Claude Code + Codex integration
  Diff viewer, markdown, approvals
  Polish and stability

Phase 2
  Relay server design + build
  Agent daemon on dev machines
  Mobile client (PWA first, then native)
  End-to-end remote access
```

Phase 2 does not start until Phase 1 is solid. The remote layer is additive — it does not change the core session/message/rendering architecture built in Phase 1.
