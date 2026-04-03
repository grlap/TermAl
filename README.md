# TermAl

An operating environment for AI coding agents.

TermAl gives you one place to run, supervise, review, and steer long-running software work performed by AI agents — structured around real agent workflows instead of raw terminal output.

## Features

- **Multi-session workspace** — run multiple agent sessions in parallel across a split-pane layout
- **Orchestrator** — visual graph editor for designing multi-agent workflows; chain sessions with transitions that fire automatically on completion and route results between agents
- **Control panel** — dockable sidebar with icon-only section tabs for Projects, Sessions, Orchestrators, Files, and Git — with project-aware context syncing
- **Multi-browser workspaces** — server-backed layout persistence lets you run independent workspace views across multiple monitors (e.g. `?workspace=planner` on one screen, `?workspace=review` on another)
- **Structured message cards** — text, commands, diffs, thinking blocks, markdown, and approval requests rendered as typed cards, not raw text
- **Streaming responses** — token-by-token output with delta events for low-latency display
- **Diff review** — unified diff cards with Monaco diff preview, click-to-open in a side pane
- **Smart pane placement** — source and diff tabs opened from the control panel land in the nearest available pane
- **Explicit approvals** — agents request permission for risky actions; you approve, reject, or set a session-wide policy
- **Prompt queueing** — send follow-up prompts while an agent is working; they run automatically in order
- **SSH remotes** — connect to remote machines over SSH tunnels; run agents on a build server while supervising from your laptop
- **Session persistence** — sessions and message history survive restart (`~/.termal/sessions.json`)
- **Filesystem and git panels** — browse files, view git status, and open source or diff views directly from the workspace
- **16 themes + 4 chrome styles** — hand-crafted color palettes, independent chrome styles (Terminal, Editorial, Studio, Blueprint), and a density control switchable at runtime

## Agent support

| Agent | Status |
|-------|--------|
| Claude Code | Supported |
| OpenAI Codex | Supported |
| Gemini CLI | Supported |
| Cursor | Supported |

## Architecture

```
Browser (React + TypeScript)        Rust Backend (axum + tokio)
┌──────────────────────┐            ┌──────────────────────────────────┐
│  Workspace           │  SSE       │  AppState                        │
│  ├── Split panes     │◄═══════════╡  ├── Sessions + message history  │
│  ├── Session chat    │  REST      │  ├── Agent child processes       │
│  ├── Diff viewer     │───────────>│  ├── Approval queues             │
│  ├── Source editor   │            │  ├── Remote registry             │
│  ├── Control panel   │            │  ├── Orchestrator templates      │
│  ├── Orchestrator    │            │  ├── Workspace layouts           │
│  ├── Filesystem      │            │  └── Persistence (~/.termal/)    │
│  └── Git status      │            │                                  │
└──────────────────────┘            │  Agent Runtimes                  │
                                    │  ├── Claude (NDJSON stdio)       │
                                    │  ├── Codex (JSON-RPC stdio)      │
                                    │  ├── Gemini (ACP stdio)          │
                                    │  └── Cursor (ACP stdio)          │
                                    └──────────────────────────────────┘
```

- **Backend:** Rust + axum + tokio on `:8787`. Spawns agents as child processes, communicates via stdin/stdout.
- **Frontend:** React 18 + TypeScript + Vite on `:4173` (dev). No external state library — state lives in `App.tsx`.
- **Real-time:** Server-Sent Events with a monotonic revision counter. Delta events for streaming; full snapshots for sync.
- **Persistence:** Sessions in `~/.termal/sessions.json`, orchestrator templates in `~/.termal/orchestrators.json`, workspace layouts in backend state.

## SSH remotes

TermAl can manage agent sessions on remote machines over SSH. The browser always connects to the local backend; remote operations are transparently proxied through persistent SSH tunnels.

### SSH authentication setup

TermAl currently launches SSH non-interactively, so the simplest setup is
key-based auth with your normal system `ssh` / `ssh-agent` flow.

If you do not already have an SSH key on your laptop:

```bash
ssh-keygen -t ed25519
```

Install that public key on the remote machine:

```bash
ssh-copy-id user@host
```

Then verify normal SSH works before configuring the remote in TermAl:

```bash
ssh user@host
```

Notes:

- `ssh-copy-id` copies your public key into the remote account's
  `~/.ssh/authorized_keys`.
- This is a public key, not a certificate.
- If `ssh-copy-id` is unavailable, append the contents of
  `~/.ssh/id_ed25519.pub` to `~/.ssh/authorized_keys` on the remote manually.

### How it works

```
Your laptop                              Remote build server
┌──────────────┐        ┌────────────────────────┐        ┌────────────────────────┐
│              │  SSE   │  Local TermAl (:8787)  │  SSH   │  Remote TermAl (:8787) │
│   Browser    │◄═══════╡                        │ tunnel │                        │
│              │  REST  │  RemoteRegistry        │───────>│  Agent Runtimes        │
│              │───────>│  ├── build-box ───────────────> │  ├── Claude            │
│              │        │  └── gpu-server ─ ...  │        │  ├── Codex             │
└──────────────┘        └────────────────────────┘        │  └── Gemini            │
                                                          └────────────────────────┘
```

1. **Configure a remote** in Settings > Remotes — give it a name, SSH host,
   user, and optional port.
2. **Create a project** bound to that remote. Sessions created inside the project run on the remote.
3. **TermAl opens an SSH tunnel** (`ssh -L {local_port}:127.0.0.1:8787`) and optionally starts a TermAl server process on the remote host.
4. All session operations (messages, approvals, stop/kill) are **proxied through the tunnel** as regular REST calls.
5. An **SSE event bridge** subscribes to the remote's `/api/events` stream, merging remote state and delta events into the local state so the UI stays live.

### SSH tunnel detail

```
Local port (47000–56999)                 Remote host
       │                                      │
       ▼                                      ▼
  ┌─────────┐    ssh -L 47001:127.0.0.1:8787 user@build-box      ┌─────────┐
  │ :47001  │ ─────────────────────────────────────────────────> │  :8787  │
  └─────────┘           encrypted tunnel                         └─────────┘
       │                                                              │
  Local TermAl                                                  Remote TermAl
  proxies REST + SSE                                            runs agents
  through this port                                             on remote fs
```

Each remote gets a dedicated local port from the 47000–56999 range. The tunnel carries all traffic: REST API calls, SSE event streams, and health checks.

**Two startup modes:**

| Mode | SSH command | When used |
|------|-----------|-----------|
| **Managed server** | `ssh ... user@host termal server` | Default — starts a TermAl server on the remote |
| **Tunnel only** | `ssh -N ...` | Fallback — assumes TermAl is already running remotely |

TermAl tries Managed server first (15 s timeout). If it fails (e.g. `termal` not installed on the remote), it falls back to Tunnel only mode automatically.

### Project-scoped routing

Remotes are bound at the **project** level, not per-session or per-action:

```
Project "backend-api"  ──  remote: build-box
  ├── Session 1  (proxied to build-box)
  ├── Session 2  (proxied to build-box)
  └── Files / Git / Reviews  (proxied to build-box)

Project "frontend"  ──  remote: local
  ├── Session 3  (runs locally)
  └── Files / Git / Reviews  (local filesystem)
```

When you create a session inside a remote-bound project, TermAl:
1. Ensures a **project binding** exists on the remote (creates one via `POST /api/projects` if needed).
2. Creates the session on the remote and stores a local **proxy record** with the mapping.
3. Starts the SSE event bridge for that remote (if not already running).

All subsequent operations — sending messages, approving actions, browsing files, viewing git status — route through the same tunnel automatically.

### State synchronization

```
Remote TermAl                    Local TermAl                     Browser
     │                                │                              │
     │── SSE state ──────────────────>│                              │
     │   (full snapshot)              │── merge + rewrite IDs ─────>│
     │                                │   (local session IDs)        │
     │── SSE delta ──────────────────>│                              │
     │   (text chunk, command update) │── apply delta + publish ───>│
     │                                │   (forwarded as local delta) │
     │                                │                              │
     │<── REST (proxied) ────────────│<── REST ─────────────────────│
     │   POST /api/sessions/3/messages│   POST /api/sessions/5/messages
     │   (remote session ID)          │   (local session ID)         │
```

The local backend is the single source of truth for the browser. It rewrites session and project IDs so the frontend sees a unified namespace — remote sessions look identical to local ones.

### Remote settings

SSH remotes currently store only the connection settings needed to reach the
machine:

- remote name
- SSH host
- optional SSH user
- optional SSH port
- enabled/disabled state

When a remote is used, TermAl first tries to start `termal server` over SSH. If
that fails, it falls back to tunnel-only mode and expects a TermAl server to
already be running on the remote host.

### Configuration reference

| Setting | Description | Default |
|---------|-------------|---------|
| `name` | Display name for the remote | *(required)* |
| `host` | SSH hostname or IP | *(required for SSH)* |
| `user` | SSH username | *(current user)* |
| `port` | SSH port | `22` |
| `enabled` | Whether the remote is available for new projects | `true` |

Remotes are configured in Settings > Remotes and persisted in `~/.termal/sessions.json` alongside session data. A built-in `local` remote is always present and cannot be removed.

## Getting started

### Prerequisites

- [Rust](https://rustup.rs/) (edition 2024)
- [Node.js](https://nodejs.org/) (see `.nvmrc` for version)
- At least one supported agent: [Claude Code](https://claude.ai/code), [Codex](https://github.com/openai/codex), [Gemini CLI](https://github.com/google-gemini/gemini-cli), or [Cursor](https://www.cursor.com/)

### Run the backend

```bash
cargo run
```

The server starts on `http://localhost:8787`.

### Run the frontend

```bash
cd ui
npm install
npm run dev
```

The UI opens on `http://localhost:4173`. API calls are proxied to the backend automatically.

### REPL mode (optional)

```bash
cargo run -- --repl
```

Runs an interactive terminal loop — reads prompts from stdin, runs one agent turn at a time. Useful for testing.

### Telegram relay (experimental)

Run the backend first, then start the Telegram relay in a separate terminal:

```bash
TERMAL_TELEGRAM_BOT_TOKEN=... \
TERMAL_TELEGRAM_PROJECT_ID=project-1 \
cargo run -- telegram
```

Optional environment variables:

- `TERMAL_TELEGRAM_CHAT_ID` — lock the relay to one Telegram chat ID
- `TERMAL_TELEGRAM_API_BASE_URL` — override the local TermAl backend URL (default `http://127.0.0.1:8787`)
- `TERMAL_TELEGRAM_PUBLIC_BASE_URL` — public URL used for `Review in TermAl` deep links
- `TERMAL_TELEGRAM_POLL_TIMEOUT_SECS` — long-poll timeout for Telegram Bot API requests

If `TERMAL_TELEGRAM_CHAT_ID` is not set, the first `/start` message links the bot to one chat and that binding is persisted in `~/.termal/telegram-bot.json`.

## Project structure

```
termal/
├── src/
│   ├── main.rs              # Entry point and CLI
│   ├── api.rs               # Axum HTTP routes
│   ├── state.rs             # AppState, sessions, persistence
│   ├── runtime.rs           # Agent runtimes (Claude, Codex, Gemini, Cursor)
│   ├── remote.rs            # SSH tunnels, remote registry, SSE bridge
│   ├── orchestrators.rs     # Orchestrator templates, instances, and transitions
│   ├── telegram.rs          # Telegram polling relay for project digests and actions
│   ├── turns.rs             # Turn lifecycle, types, shared structures
│   └── tests.rs             # Rust unit tests
├── ui/
│   ├── src/
│   │   ├── App.tsx              # Main React component
│   │   ├── api.ts               # API client
│   │   ├── types.ts             # Shared TypeScript types
│   │   ├── workspace.ts         # Pane/tab/split state
│   │   ├── workspace-storage.ts # Multi-browser workspace view ID and layout persistence
│   │   ├── live-updates.ts      # Delta event application
│   │   ├── themes/              # 16 color themes + 4 chrome style presets
│   │   └── panels/
│   │       ├── AgentSessionPanel.tsx                # Chat session view
│   │       ├── ControlPanelSurface.tsx              # Dockable sidebar with section tabs
│   │       ├── OrchestratorTemplatesPanel.tsx        # Visual canvas editor for workflows
│   │       ├── OrchestratorTemplateLibraryPanel.tsx  # Template library and instance management
│   │       ├── SessionCanvasPanel.tsx               # Session graph overview
│   │       ├── SourcePanel.tsx                      # Source file viewer
│   │       ├── DiffPanel.tsx                        # Diff viewer
│   │       ├── FileSystemPanel.tsx                  # Filesystem browser
│   │       ├── GitStatusPanel.tsx                   # Git status and diff tree
│   │       └── InstructionDebuggerPanel.tsx         # Agent instruction tracing
│   └── vite.config.ts          # Dev proxy: /api → :8787
├── docs/
│   ├── architecture.md      # Full architecture reference
│   ├── vision.md            # Product vision
│   ├── roadmap.md           # Phased roadmap
│   ├── bugs.md              # Bug tracker and implementation backlog
│   └── features/            # Feature briefs (orchestration, workspaces, agent integrations, etc.)
├── Cargo.toml
└── Cargo.lock
```

## Orchestrator

The orchestrator lets you design reusable multi-agent workflows as directed graphs. Each node is an agent session; each edge is a transition that fires when a session completes.

### Template design

Open the orchestrator canvas to build a workflow visually:

- **Add session cards** — each card defines an agent, model, and instruction prompt
- **Draw transitions** — drag between anchor points on cards to create edges; set a trigger (`OnCompletion`), result mode, and optional prompt template
- **Save as a template** — templates are persisted to `~/.termal/orchestrators.json` and reusable across projects

### Runtime

Launch an orchestrator instance from a template. TermAl creates the sessions, starts the first one, and then:

1. When a session reaches `prompt_ready`, the orchestrator evaluates outgoing transitions.
2. The transition assembles a follow-up prompt (optionally including the source session's last response or a summary).
3. The follow-up prompt is delivered to the target session, which starts automatically.

Instances can be **paused**, **resumed**, or **stopped** from the control panel or via REST API.

### Transition settings

| Setting | Options | Description |
|---------|---------|-------------|
| Trigger | `OnCompletion` | Fires when the source session finishes its turn |
| Result mode | `None`, `LastResponse`, `Summary`, `SummaryAndLastResponse` | What context to include in the delivered prompt |
| Input mode | `Queue`, `Consolidate` | How multiple inbound transitions are handled |
| Prompt template | Free text with `{{result}}` placeholder | Custom prompt wrapping the result |

## Roadmap

TermAl evolves in four phases:

| Phase | Goal | Status |
|-------|------|--------|
| 1 — Local AI terminal | Reliable local control room for agent sessions | **In progress** |
| 2 — Remote PC access | Connect to running sessions from another computer | **In progress** |
| 3 — Mobile access | Supervise, approve, and review from a phone | Planned |
| 4 — Remote pair programming | Two humans collaborating around the same agent session | Planned |

See [`docs/roadmap.md`](docs/roadmap.md) for the full breakdown.

## Docs

- [`docs/architecture.md`](docs/architecture.md) — system design, API reference, agent protocol details
- [`docs/vision.md`](docs/vision.md) — product framing and guiding principles
- [`docs/roadmap.md`](docs/roadmap.md) — phased roadmap
- [`docs/bugs.md`](docs/bugs.md) — implementation backlog
- [`docs/features/`](docs/features/) — feature briefs including:
  - [Orchestration](docs/features/orchestration.md) — multi-agent workflow design
  - [Multi-browser workspaces](docs/features/multi-browser-workspaces.md) — server-backed layout persistence
  - [Diff review workflow](docs/features/diff-review-workflow.md) — structured diff review
  - [Agent integrations](docs/features/agent-integration-comparison.md) — Claude, Codex, Gemini, Cursor comparison
  - [Project-scoped remotes](docs/features/project-scoped-remotes.md) — remote binding at project level
