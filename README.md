# TermAl

An operating environment for AI coding agents.

TermAl gives you one place to run, supervise, review, and steer long-running software work performed by AI agents — structured around real agent workflows instead of raw terminal output.

## Features

- **Multi-session workspace** — run multiple agent sessions in parallel across a split-pane layout
- **Structured message cards** — text, commands, diffs, thinking blocks, markdown, and approval requests rendered as typed cards, not raw text
- **Streaming responses** — token-by-token output with delta events for low-latency display
- **Diff review** — unified diff cards with Monaco diff preview, click-to-open in a side pane
- **Explicit approvals** — agents request permission for risky actions; you approve, reject, or set a session-wide policy
- **Prompt queueing** — send follow-up prompts while an agent is working; they run automatically in order
- **Session persistence** — sessions and message history survive restart (`~/.termal/sessions.json`)
- **Filesystem and git panels** — browse files, view git status, and open source or diff views directly from the workspace
- **17 themes** — hand-crafted CSS themes, switchable at runtime

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
│  ├── Source editor   │            │  └── Persistence (~/.termal/)    │
│  ├── Filesystem      │            │                                  │
│  └── Git status      │            │  Agent Runtimes                  │
└──────────────────────┘            │  ├── Claude (NDJSON stdio)       │
                                    │  ├── Codex (JSON-RPC stdio)      │
                                    │  ├── Gemini (ACP stdio)          │
                                    │  └── Cursor (ACP stdio)          │
                                    └──────────────────────────────────┘
```

- **Backend:** Rust + axum + tokio on `:8787`. Spawns agents as child processes, communicates via stdin/stdout.
- **Frontend:** React 18 + TypeScript + Vite on `:4173` (dev). No external state library — state lives in `App.tsx`.
- **Real-time:** Server-Sent Events with a monotonic revision counter. Delta events for streaming; full snapshots for sync.
- **Persistence:** Single JSON file at `~/.termal/sessions.json`.

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

## Project structure

```
termal/
├── src/
│   └── main.rs              # Rust backend (~7600 lines)
├── ui/
│   ├── src/
│   │   ├── App.tsx          # Main React component (~4500 lines)
│   │   ├── api.ts           # API client
│   │   ├── types.ts         # Shared TypeScript types
│   │   ├── workspace.ts     # Pane/tab/split state
│   │   ├── live-updates.ts  # Delta event application
│   │   ├── themes/          # 17 CSS theme files
│   │   └── panels/          # AgentSession, Source, Diff, Filesystem, Git panels
│   └── vite.config.ts       # Dev proxy: /api → :8787
├── docs/
│   ├── architecture.md      # Full architecture reference
│   ├── vision.md            # Product vision
│   ├── roadmap.md           # Phased roadmap
│   └── bugs.md              # Bug tracker and implementation backlog
├── Cargo.toml
└── Cargo.lock
```

## Roadmap

TermAl evolves in four phases:

| Phase | Goal | Status |
|-------|------|--------|
| 1 — Local AI terminal | Reliable local control room for agent sessions | **In progress** |
| 2 — Remote PC access | Connect to running sessions from another computer | Planned |
| 3 — Mobile access | Supervise, approve, and review from a phone | Planned |
| 4 — Remote pair programming | Two humans collaborating around the same agent session | Planned |

See [`docs/roadmap.md`](docs/roadmap.md) for the full breakdown.

## Docs

- [`docs/architecture.md`](docs/architecture.md) — system design, API reference, agent protocol details
- [`docs/vision.md`](docs/vision.md) — product framing and guiding principles
- [`docs/roadmap.md`](docs/roadmap.md) — phased roadmap
- [`docs/bugs.md`](docs/bugs.md) — implementation backlog
